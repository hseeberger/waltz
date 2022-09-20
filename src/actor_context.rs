use crate::{ActorId, ActorRef, Handler, MsgOrSignal, StateOrStop};
use futures::FutureExt;
use std::{future::Future, panic::AssertUnwindSafe, sync::Arc};
use tokio::{
    sync::{mpsc, watch},
    task,
};
use tracing::{debug, error};

/// Contextual methods for a given actor, provided as handler parameter.
pub struct ActorContext<M> {
    self_ref: ActorRef<M>,
}

impl<M> ActorContext<M>
where
    M: Send + 'static,
{
    /// The reference for the actor itself.
    pub fn self_ref(&self) -> &ActorRef<M> {
        &self.self_ref
    }

    /// Spawn a child actor with the given handler and initial state.
    pub async fn spawn<N, H, S, I, F>(&self, mut handler: H, init: I) -> ActorRef<N>
    where
        N: Send + 'static,
        H: Handler<Msg = N, State = S> + Send + 'static,
        S: Send + 'static,
        I: FnOnce(Arc<ActorContext<N>>) -> F,
        F: Future<Output = S>,
    {
        let (terminated_in, terminated_out) = watch::channel::<ActorId>(ActorId::nil());
        let (mailbox_in, mut mailbox_out) = mpsc::channel::<MsgOrSignal<N>>(42);

        let actor_ref = ActorRef::new(mailbox_in, terminated_out);
        let id = actor_ref.id();

        let ctx = ActorContext::new(actor_ref.clone());
        let ctx = Arc::new(ctx);
        let mut state = init(ctx.clone()).await;

        task::spawn(async move {
            while let Some(msg) = mailbox_out.recv().await {
                let receive = handler.receive(&ctx, msg, state);
                let receive = AssertUnwindSafe(receive).catch_unwind();
                match receive.await {
                    Ok(StateOrStop::State(next_state)) => state = next_state,
                    Ok(StateOrStop::Stop) => {
                        debug!("Stopping actor {id} as decided by handler");
                        break;
                    }
                    Err(e) => {
                        error!("Stopping actor {id}, because handler failed: {e:?}");
                        break;
                    }
                }
            }

            if let Err(e) = terminated_in.send(id) {
                error!("Could not send Terminated signal for actor {id}: {e}");
            };
        });

        actor_ref
    }

    /// Watch another actor, i.e. receive a [MsgOrSignal::Terminated] signal if that actor has
    /// stopped.
    pub fn watch<N>(&self, other: ActorRef<N>) {
        let self_ref = self.self_ref().clone();
        let mut terminated = other.terminated;
        task::spawn(async move {
            match terminated.changed().await {
                Ok(_) => {
                    let id = *terminated.borrow();
                    let _ = self_ref.mailbox.send(MsgOrSignal::Terminated(id)).await;
                }
                Err(e) => error!("Cannot receive Terminated signal: {e}"),
            }
        });
    }

    pub(crate) fn new(self_ref: ActorRef<M>) -> Self {
        Self { self_ref }
    }
}
