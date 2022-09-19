use crate::{spawn, terminated, ActorContext, ActorRef, Handler};
use std::future::Future;
use terminated::terminated;
use thiserror::Error;
use tracing::error;

#[derive(Debug, Error)]
/// Errors for this module.
pub enum Error {
    /// Unexpected failure during watching the guardian actor.
    #[error("unexpected failure during watching guardian actor")]
    WatchGuardian { source: terminated::Error },
}

pub struct ActorSystem<M>(ActorRef<M>);

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
        let guardian = spawn(handler, init).await;
        Self(guardian)
    }

    pub fn guardian(&self) -> &ActorRef<M> {
        &self.0
    }

    pub async fn terminated(&self) -> Result<(), Error> {
        terminated(self.0.clone())
            .await
            .map_err(|source| Error::WatchGuardian { source })
    }
}
