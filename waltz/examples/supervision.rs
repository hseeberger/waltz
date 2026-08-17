//! A flaky `Loader` under the `Restart` supervision strategy: a failure re-runs `init` for a fresh
//! state after an exponentially growing backoff, while the actor value and the mailbox are
//! retained, so the messages queued behind a toxic one still get processed; only the count is
//! rebuilt from zero. More than `max_restarts` failures in a streak would stop the loader instead,
//! escalating to the watching root.
//!
//! The output is printed to stdout and waltz logs the failures and restarts to stderr; the log
//! level is configured via `RUST_LOG`, e.g.
//! `RUST_LOG=waltz=debug cargo run --quiet -p waltz --example supervision`.

use anyhow::Context;
use std::{convert::Infallible, io, num::NonZeroU32, time::Duration};
use thiserror::Error;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
use waltz::{
    Actor, ActorConfig, ActorContext, ActorSystem, Backoff, Control, Incoming, Nothing,
    RestartPolicy, SupervisionStrategy,
};

const TOXIC: u32 = 13;
const MAX_RESTARTS: NonZeroU32 = NonZeroU32::new(3).expect("3 is not zero");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let backoff = Backoff::new(Duration::from_millis(50), Duration::from_secs(1))
        .context("backoff bounds for the loader")?;
    let loader_config = ActorConfig::default().with_supervision_strategy(
        SupervisionStrategy::Restart(RestartPolicy::new(MAX_RESTARTS).with_backoff(backoff)),
    );

    let system = ActorSystem::new(Overseer { loader_config });

    system
        .terminated()
        .await
        .context("awaiting actor system termination")
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .flatten_event(true)
                .with_writer(io::stderr),
        )
        .init();
}

struct Overseer {
    loader_config: ActorConfig,
}

impl Actor for Overseer {
    type Message = Nothing;
    type State = ();
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        let loader = context.spawn_with_config(Loader, self.loader_config);
        context.watch(&loader);

        // The two toxic values cause two restarts; everything queued behind them still loads.
        for value in [1, 2, TOXIC, 3, TOXIC, 4] {
            loader.tell(Message::Load(value));
        }
        loader.tell(Message::Finish);

        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        _: Incoming<Self::Message>,
        _: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        // With `Nothing` as message type the only possible incoming is the terminated signal.
        println!("## Loader finished");
        Ok(Control::Stop)
    }
}

struct Loader;

impl Actor for Loader {
    type Message = Message;
    type State = u64;
    type Error = ToxicValue;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        println!("## Loader initialized, count starts over at 0");
        Ok(0)
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        count: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        let Incoming::Message(message) = incoming else {
            unreachable!("the loader watches no actor, hence never gets a terminated signal")
        };

        match message {
            // The error escalates to supervision, which restarts the loader with backoff.
            Message::Load(TOXIC) => Err(ToxicValue(TOXIC)),

            Message::Load(value) => {
                let count = count + 1;
                println!("## Loaded {value}, count is {count}");
                Ok(Control::Continue(count))
            }

            Message::Finish => Ok(Control::Stop),
        }
    }
}

enum Message {
    Load(u32),
    Finish,
}

#[derive(Debug, Error)]
#[error("toxic value {0}")]
struct ToxicValue(u32);
