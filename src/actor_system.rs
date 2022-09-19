use crate::{ActorContext, ActorId, ActorRef, Handler, MsgOrSignal, NotUsed, StateOrStop};
use async_trait::async_trait;
use std::future::Future;
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task,
};
use tracing::{debug, error};

#[derive(Debug, Error)]
/// Errors for this module.
pub enum Error {
    /// Unexpected failure during watching the guardian actor.
    #[error("unexpected failure during watching guardian actor")]
    WatchGuardian { source: oneshot::error::RecvError },
}

pub struct ActorSystem<M> {
    guardian: ActorRef<M>,
    terminated: oneshot::Receiver<()>,
}

impl<M> ActorSystem<M>
where
    M: Send + 'static,
{
    // Create an actor system by passing the handler and state initializer for the guardian
    // (root) actor.
    pub async fn new<H, S, I, F>(handler: H, init: I) -> Self
    where
        H: Handler<Msg = M, State = S> + Send + 'static,
        S: Send + 'static,
        I: FnOnce(ActorContext<M>) -> F,
        F: Future<Output = (ActorContext<M>, S)>,
    {
        let (guardian, terminated) = spawn_root(handler, init).await;
        Self {
            guardian,
            terminated,
        }
    }

    pub fn guardian(&self) -> &ActorRef<M> {
        &self.guardian
    }

    pub async fn terminated(self) -> Result<(), Error> {
        self.terminated
            .await
            .map_err(|source| Error::WatchGuardian { source })
    }
}

struct Root;

#[async_trait]
impl Handler for Root {
    type Msg = NotUsed;

    type State = oneshot::Sender<()>;

    async fn receive(
        &mut self,
        _: &ActorContext<Self::Msg>,
        msg: MsgOrSignal<Self::Msg>,
        state: Self::State,
    ) -> StateOrStop<Self::State> {
        match msg {
            MsgOrSignal::Terminated(_) => {
                debug!("Stoping because guardian has terminated");
                let _ = state.send(());
                StateOrStop::Stop
            }
            MsgOrSignal::Msg(_) => {
                error!("Received unexpected message");
                let _ = state.send(());
                StateOrStop::Stop
            }
        }
    }
}

async fn spawn_root<M, H, S, I, F>(
    guardian_handler: H,
    guardian_init: I,
) -> (ActorRef<M>, oneshot::Receiver<()>)
where
    M: Send + 'static,
    H: Handler<Msg = M, State = S> + Send + 'static,
    S: Send + 'static,
    I: FnOnce(ActorContext<M>) -> F,
    F: Future<Output = (ActorContext<M>, S)>,
{
    let (terminated_sender, terminated_receiver) = oneshot::channel::<()>();

    let (_, terminated_out) = watch::channel::<ActorId>(ActorId::nil());
    let (mailbox_in, mut mailbox_out) = mpsc::channel::<MsgOrSignal<NotUsed>>(1);

    let root = ActorRef::new(mailbox_in, terminated_out);
    let ctx = ActorContext::new(root);

    let guardian = ctx.spawn(guardian_handler, guardian_init).await;
    ctx.watch(guardian.clone());

    let mut root = Root;
    task::spawn(async move {
        if let Some(msg) = mailbox_out.recv().await {
            let receive = root.receive(&ctx, msg, terminated_sender);
            match receive.await {
                StateOrStop::Stop => {
                    debug!("Stopping root actor");
                }
                _other => error!("Unexpected receive result"),
            }
        }
    });

    (guardian, terminated_receiver)
}
