use crate::{ActorId, MsgOrSignal};
use tokio::sync::{mpsc, watch};

/// A shareable reference to an actor, allowing to access its ID and send messages to it.
pub struct ActorRef<M> {
    id: ActorId,
    pub(crate) mailbox: mpsc::Sender<MsgOrSignal<M>>,
    pub(crate) terminated: watch::Receiver<ActorId>,
}

impl<M> ActorRef<M> {
    /// The ID of the actor represented by this `ActorRef`.
    pub fn id(&self) -> ActorId {
        self.id
    }

    /// Send a message to the actor represented by this `ActorRef`. If the actor has stopped, this
    /// will not happen. Also, even if the message is delivered to the actor, it might stop before
    /// processing it.
    pub async fn tell(&self, msg: M) {
        let _ = self.mailbox.send(MsgOrSignal::Msg(msg)).await;
    }

    pub(crate) fn new(
        mailbox: mpsc::Sender<MsgOrSignal<M>>,
        terminated: watch::Receiver<ActorId>,
    ) -> Self {
        Self {
            id: ActorId::new(),
            mailbox,
            terminated,
        }
    }
}

impl<M> Clone for ActorRef<M> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            mailbox: self.mailbox.clone(),
            terminated: self.terminated.clone(),
        }
    }
}
