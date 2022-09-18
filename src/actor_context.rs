use crate::{ActorRef, MsgOrSignal};
use tokio::task;
use tracing::error;

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
