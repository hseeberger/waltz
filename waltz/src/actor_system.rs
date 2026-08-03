use crate::{
    Actor, ActorConfig, ActorId, ActorRef,
    actor_context::spawn,
    actor_ref::WatchTarget,
    mailbox::{ActorTerminated, TerminatedSink, Watcher},
    sync::lock,
};
use derive_more::Debug;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::{oneshot, watch};

/// An actor system, hosting the tree of actors below its root actor.
///
/// Dropping an actor system does not stop its actors: the root actor stops on its own terms and
/// the tree keeps running detached; dropping merely forfeits [ActorSystem::terminated].
#[must_use = "dropping an actor system does not stop its actors"]
#[derive(Debug)]
pub struct ActorSystem<M> {
    root: ActorRef<M>,

    #[debug(skip)]
    terminated_rx: oneshot::Receiver<()>,
}

impl<M> ActorSystem<M>
where
    M: Send + 'static,
{
    /// Create an actor system by giving the [Actor] for the root actor, using the default
    /// [ActorConfig].
    ///
    /// # Panics
    /// Panics if called outside of a Tokio runtime.
    pub fn new<A>(actor: A) -> Self
    where
        A: Actor<Message = M> + Send + 'static,
        A::State: Send + 'static,
    {
        Self::with_config(actor, ActorConfig::default())
    }

    /// Create an actor system by giving the [Actor] and [ActorConfig] for the root actor.
    ///
    /// # Panics
    /// Panics if called outside of a Tokio runtime.
    pub fn with_config<A>(actor: A, config: ActorConfig) -> Self
    where
        A: Actor<Message = M> + Send + 'static,
        A::State: Send + 'static,
    {
        let (root, terminated_rx) = spawn_root(actor, config);
        Self {
            root,
            terminated_rx,
        }
    }

    /// The reference for the root actor.
    pub fn root(&self) -> &ActorRef<M> {
        &self.root
    }

    /// Wait until the root actor and all its descendants have terminated.
    pub async fn terminated(self) -> Result<(), Error> {
        self.terminated_rx.await?;
        Ok(())
    }
}

/// Errors possibly returned by [ActorSystem::terminated].
#[derive(Debug, Error)]
pub enum Error {
    /// Unexpected failure during watching the root actor.
    #[error("unexpected failure during watching root actor")]
    WatchRoot(#[from] oneshot::error::RecvError),
}

fn spawn_root<M, A>(root_actor: A, config: ActorConfig) -> (ActorRef<M>, oneshot::Receiver<()>)
where
    M: Send + 'static,
    A: Actor<Message = M> + Send + 'static,
    A::State: Send + 'static,
{
    let (terminated_tx, terminated_rx) = oneshot::channel();
    let (stopping_tx, stopping_rx) = watch::channel(());

    let root = spawn(stopping_rx, root_actor, config);

    let sink = Arc::new(RootTerminatedSink {
        terminated_tx: Mutex::new(Some(terminated_tx)),
        _stopping_tx: stopping_tx,
    });
    let registration = match root.watch_target() {
        WatchTarget::Local(registry) => registry.add(Watcher::new(ActorId::new(), sink.clone())),

        #[cfg(feature = "remote")]
        WatchTarget::Remote(_) => unreachable!("the root actor is local"),
    };
    if registration.is_err() {
        sink.send_terminated(root.actor_id())
            .expect("a sink whose registration failed was never signaled");
    }

    (root, terminated_rx)
}

/// `_stopping_tx` keeps the root actor running: living in the root's own watcher registry, it is
/// dropped only once termination has signaled the watchers.
struct RootTerminatedSink {
    terminated_tx: Mutex<Option<oneshot::Sender<()>>>,
    _stopping_tx: watch::Sender<()>,
}

impl TerminatedSink for RootTerminatedSink {
    fn send_terminated(&self, _actor_id: ActorId) -> Result<(), ActorTerminated> {
        let terminated_tx = lock(&self.terminated_tx).take().ok_or(ActorTerminated)?;
        let _ = terminated_tx.send(());

        Ok(())
    }
}
