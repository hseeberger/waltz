//! A counter actor showing the two send modes: `tell` fires increments without awaiting anything,
//! `ask` sends a request carrying a `ReplyTo` and awaits the reply under a timeout. The mailbox is
//! FIFO, so the reply reflects every increment told before the ask.

use anyhow::Context;
use std::{convert::Infallible, time::Duration};
use waltz::{Actor, ActorContext, ActorSystem, Control, Incoming, ReplyTo};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let system = ActorSystem::new(Counter);

    for _ in 0..3 {
        system.root().tell(Message::Increment);
    }

    // Queued behind the increments above, so the reply is the full count.
    let count = system
        .root()
        .ask(Duration::from_secs(1), Message::Get)
        .await
        .context("asking for the count")?;
    println!("The count is: {count}");

    system.root().tell(Message::Stop);

    system
        .terminated()
        .await
        .context("awaiting actor system termination")
}

struct Counter;

impl Actor for Counter {
    type Message = Message;
    type State = u64;
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(0)
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        count: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        let Incoming::Message(message) = incoming else {
            unreachable!("the counter watches no actor, hence never gets a terminated signal")
        };

        match message {
            Message::Increment => Ok(Control::Continue(count + 1)),

            Message::Get(reply_to) => {
                reply_to.reply(count);
                Ok(Control::Continue(count))
            }

            Message::Stop => Ok(Control::Stop),
        }
    }
}

enum Message {
    Increment,
    Get(ReplyTo<u64>),
    Stop,
}
