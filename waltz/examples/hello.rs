//! The minimal waltz program: a root actor which greets for the one message it receives and then
//! stops, terminating the actor system.
//!
//! Keep this example in sync with the getting started snippet in the README, line by line.

use anyhow::Context;
use std::convert::Infallible;
use waltz::{Actor, ActorContext, ActorSystem, Control, Incoming};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let system = ActorSystem::new(Greeter);
    system.root().tell(Greet("Waltz".to_string()));
    system
        .terminated()
        .await
        .context("awaiting actor system termination")
}

struct Greeter;

impl Actor for Greeter {
    type Message = Greet;
    type State = ();
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        _: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        if let Incoming::Message(Greet(name)) = incoming {
            println!("Hello, {name}!");
        }
        Ok(Control::Stop)
    }
}

struct Greet(String);
