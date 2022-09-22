use crate::{ActorId, ActorRef, Handler, MsgOrSignal, StateOrStop};
use futures::FutureExt;
use std::{future::Future, panic::AssertUnwindSafe, sync::Arc};
use tokio::{
    select,
    sync::{mpsc, watch},
    task,
};
use tracing::{debug, error};

#[macro_export]
macro_rules! spawn {
    ($ctx:ident, $handler:expr, |$c:ident| $state:expr) => {
        $ctx.spawn(
            $handler,
            $crate::CONFIG.default_mailbox_size,
            |$c| async move { $state },
        )
    };
    ($ctx:ident, $handler:expr, $state:expr) => {
        $ctx.spawn(
            $handler,
            $crate::CONFIG.default_mailbox_size,
            |_| async move { $state },
        )
    };
}

/// Contextual methods for a given actor, provided as handler parameter.
pub struct ActorContext<M> {
    self_ref: ActorRef<M>,
    stop_by_parent: watch::Receiver<bool>,
}

impl<M> ActorContext<M>
where
    M: Send + 'static,
{
    /// The reference for the actor itself.
    pub fn self_ref(&self) -> &ActorRef<M> {
        &self.self_ref
    }

    /// Spawn a child actor with the given handler, mailbox size (which must be positive) and
    /// initial state.
    ///
    /// # Panics
    /// Panics if the given mailbox size is zero.
    pub async fn spawn<N, H, S, I, F>(
        &self,
        mut handler: H,
        mailbox_size: usize,
        init: I,
    ) -> ActorRef<N>
    where
        N: Send + 'static,
        H: Handler<Msg = N, State = S> + Send + 'static,
        S: Send + 'static,
        I: FnOnce(Arc<ActorContext<N>>) -> F,
        F: Future<Output = S>,
    {
        let (stop_children, stop_by_parent) = watch::channel::<bool>(false);
        let (terminated_in, terminated_out) = watch::channel::<ActorId>(ActorId::nil());
        let (mailbox_in, mut mailbox_out) = mpsc::channel::<MsgOrSignal<N>>(mailbox_size);

        let actor_ref = ActorRef::new(mailbox_in, terminated_out);
        let id = actor_ref.id();

        let ctx = ActorContext::new(actor_ref.clone(), stop_by_parent);
        let ctx = Arc::new(ctx);
        let mut state = init(ctx.clone()).await;

        let mut stop_by_parent = self.stop_by_parent.clone();
        task::spawn(async move {
            loop {
                select! {
                    biased;

                    // Listen to stop signal from parent
                    _ = stop_by_parent.changed() => {
                        debug!(actor_id = display(id), "Received stop signal from parent");
                        break
                    },

                    // Receive message from mailbox and invoke handler
                    msg = mailbox_out.recv() => {
                        match msg {
                            Some(msg) => {
                                let receive = handler.receive(&ctx, msg, state);
                                let receive = AssertUnwindSafe(receive).catch_unwind();
                                match receive.await {
                                    Ok(StateOrStop::State(next_state)) => {
                                        state = next_state;
                                    }
                                    Ok(StateOrStop::Stop) => {
                                        debug!(actor_id = display(id), "Stopping as decided by handler");
                                        break;
                                    }
                                    Err(e) => {
                                        error!(
                                            actor_id = display(id),
                                            error = debug(e),
                                            "Stopping, because handler failed"
                                        );
                                        break;
                                    }
                                }
                            }
                            None => break,
                        }
                    }
                };
            }

            if let Err(e) = stop_children.send(true) {
                error!(
                    actor_id = display(id),
                    error = debug(e),
                    "Cannot send stop signal to child actors"
                );
            }

            // Dropping context to drop its `stop_by_parent`, but "save" its `terminated`, which is
            // needed for the below termination signal sender
            let _terminated = ctx.self_ref().terminated.clone();
            drop(ctx);
            // Once all child actors are terminated, their `stop_by_parent` clones are dropped, i.e.
            // all clones, and `stop_children` is closed
            stop_children.closed().await;
            debug!(actor_id = display(id), "All child actors terminated");

            // Send terminated signal to watchers
            if let Err(e) = terminated_in.send(id) {
                error!(
                    actor_id = display(id),
                    error = debug(e),
                    "Cannot send terminated signal"
                );
            };
            debug!(actor_id = display(id), "Terminated");
        });

        actor_ref
    }

    /// Watch another actor, i.e. receive a [MsgOrSignal::Terminated] signal if that actor has
    /// stopped.
    pub fn watch<N>(&self, other: ActorRef<N>) {
        let self_ref = self.self_ref().clone();
        let id = self_ref.id();
        let other_id = other.id();
        let mut other_terminated = other.terminated;

        task::spawn(async move {
            match other_terminated.changed().await {
                Ok(_) => {
                    let id = *other_terminated.borrow();
                    let _ = self_ref.mailbox.send(MsgOrSignal::Terminated(id)).await;
                }
                Err(e) => error!(
                    actor_id = display(id),
                    other_id = display(other_id),
                    error = debug(e),
                    "Cannot receive terminated signal"
                ),
            }
        });
    }

    pub(crate) fn new(self_ref: ActorRef<M>, stop_by_parent: watch::Receiver<bool>) -> Self {
        Self {
            self_ref,
            stop_by_parent,
        }
    }
}
