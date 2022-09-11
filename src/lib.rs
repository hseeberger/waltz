use async_trait::async_trait;
use tokio::{
    sync::mpsc::{channel, Sender},
    task,
};

/// A stateful handler for received messages.
#[async_trait]
pub trait Handler {
    /// The type of the received messages.
    type Msg;

    /// The type of the state. For stateless handlers use `()` or similar.
    type State;

    /// Receive a message, apply it to the current state and return the new state.
    async fn receive(&mut self, msg: Self::Msg, state: Self::State) -> Self::State;
}

pub struct ActorRef<M> {
    mailbox: Sender<M>,
}

impl<M> ActorRef<M> {
    pub async fn tell(&self, msg: M) {
        let _ = self.mailbox.send(msg).await;
    }
}

impl<M> Clone for ActorRef<M> {
    fn clone(&self) -> Self {
        Self {
            mailbox: self.mailbox.clone(),
        }
    }
}

/// Spawn an actor with the given handler and initial state. The returned [ActorRef] can be used
/// to send messages to this actor.
pub fn spawn<M, H, S>(handler: H, initial_state: S) -> ActorRef<M>
where
    M: Send + 'static,
    H: Handler<Msg = M, State = S> + Send + 'static,
    S: Send + 'static,
{
    let (sender, mut receiver) = channel(42);

    task::spawn(async move {
        let mut actor = Actor {
            state: initial_state,
            handler,
        };

        loop {
            match receiver.recv().await {
                Some(msg) => {
                    actor.state = actor.handler.receive(msg, actor.state).await;
                }
                None => break,
            }
        }
    });

    ActorRef { mailbox: sender }
}

struct Actor<M, H, S>
where
    H: Handler<Msg = M, State = S>,
{
    state: S,
    handler: H,
}

#[cfg(test)]
mod tests {
    #[test]
    fn test() {}
}
