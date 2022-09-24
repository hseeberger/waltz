use crate::{ActorContext, ActorId, ActorRef, Handler, MsgOrSignal, NotUsed, StateOrStop};
use async_trait::async_trait;
use std::{future::Future, sync::Arc};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task,
};
use tracing::{debug, error};

#[macro_export]
macro_rules! system {
    ($handler:expr, |$c:ident| $state:expr) => {
        ActorSystem::new(
            $handler,
            $crate::CONFIG.default_mailbox_size,
            |$c| async move { $state },
        )
    };
    ($handler:expr, $state:expr) => {
        ActorSystem::new(
            $handler,
            $crate::CONFIG.default_mailbox_size,
            |_| async move { $state },
        )
    };
}

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
    /// Create an actor system by giving the handler, mailbox size (which must be positive) and
    /// initial state for the guardian actor.
    ///
    /// # Panics
    /// Panics if the given mailbox size is zero.
    pub async fn new<H, S, I, F>(handler: H, mailbox_size: usize, init: I) -> Self
    where
        H: Handler<Msg = M, State = S> + Send + 'static,
        S: Send + 'static,
        I: FnOnce(Arc<ActorContext<M>>) -> F,
        F: Future<Output = S>,
    {
        let (guardian, terminated) = spawn_root(handler, mailbox_size, init).await;
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
                debug!("Stoping root actor, because guardian has terminated");
                let _ = state.send(());
                StateOrStop::Stop
            }
            MsgOrSignal::Msg(_) => {
                error!("Root actor received unexpected message");
                let _ = state.send(());
                StateOrStop::Stop
            }
        }
    }
}

async fn spawn_root<M, H, S, I, F>(
    guardian_handler: H,
    mailbox_size: usize,
    guardian_init: I,
) -> (ActorRef<M>, oneshot::Receiver<()>)
where
    M: Send + 'static,
    H: Handler<Msg = M, State = S> + Send + 'static,
    S: Send + 'static,
    I: FnOnce(Arc<ActorContext<M>>) -> F,
    F: Future<Output = S>,
{
    let (system_terminated_sender, system_terminated_receiver) = oneshot::channel::<()>();

    let (stop_children, stop_by_parent) = watch::channel::<bool>(false);
    let (_, terminated_out) = watch::channel::<ActorId>(ActorId::nil());
    let (mailbox_in, mut mailbox_out) = mpsc::channel::<MsgOrSignal<NotUsed>>(1);

    let root = ActorRef::new(mailbox_in, terminated_out);
    let ctx = ActorContext::new(root, stop_by_parent);

    let guardian = ctx
        .spawn(guardian_handler, mailbox_size, guardian_init)
        .await;
    ctx.watch(guardian.clone());

    let mut root = Root;
    task::spawn(async move {
        if let Some(msg) = mailbox_out.recv().await {
            let receive = root.receive(&ctx, msg, system_terminated_sender);
            let _ = receive.await;
        }
        // This must not be dropped before, even though never used, else the receivers fail
        drop(stop_children)
    });

    (guardian, system_terminated_receiver)
}
