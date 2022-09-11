use async_trait::async_trait;
use tokio::{
    sync::mpsc::{channel, Sender},
    task,
};

#[async_trait]
pub trait Handler<M> {
    async fn receive(&mut self, msg: M);
}

pub fn spawn<M, H>(mut handler: H) -> ActorRef<M>
where
    M: Send + 'static,
    H: Handler<M> + Send + 'static,
{
    let (sender, mut receiver) = channel(42);

    task::spawn(async move {
        loop {
            match receiver.recv().await {
                Some(msg) => handler.receive(msg).await,
                None => break,
            }
        }
    });

    ActorRef { mailbox: sender }
}

#[derive(Clone)]
pub struct ActorRef<M> {
    mailbox: Sender<M>,
}

impl<M> ActorRef<M> {
    pub async fn tell(&self, msg: M) {
        let _ = self.mailbox.send(msg).await;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test() {}
}
