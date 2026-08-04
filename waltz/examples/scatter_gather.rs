//! In this example the root actor scatters a workload across worker actors and gathers their
//! partial results: each worker sums up its shard, tells the result back and stops. The root
//! watches its workers; a terminated signal is ordered behind everything the terminated actor has
//! delivered to the watcher, so receiving one proves that worker's partial result has already been
//! added.
//! Hence, once all workers have terminated, the root can print the total and stop, which terminates
//! the actor system.
//!
//! The total is printed to stdout and waltz logs to stderr; the log level is configured via
//! `RUST_LOG`, e.g. `RUST_LOG=waltz=debug cargo run --quiet -p waltz --example scatter_gather`.

use anyhow::Context;
use logforth::{append::Stderr, filter::rustlog::RustLogFilterBuilder, layout::JsonLayout};
use std::{convert::Infallible, ops::Range};
use waltz::{Actor, ActorContext, ActorRef, ActorSystem, Control, Incoming};

const SHARDS: [Range<u64>; 4] = [1..26, 26..51, 51..76, 76..101];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();

    let system = ActorSystem::new(Gatherer);

    system
        .terminated()
        .await
        .context("awaiting actor system termination")
}

fn init_logging() {
    logforth::starter_log::builder()
        .dispatch(|dispatch| {
            dispatch
                .filter(RustLogFilterBuilder::from_default_env().build())
                .append(Stderr::default().with_layout(JsonLayout::default()))
        })
        .apply();
}

struct Gatherer;

impl Actor for Gatherer {
    type Message = Partial;
    type State = Gathering;
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        for shard in SHARDS {
            let worker = context.spawn(Worker);
            context.watch(&worker);
            worker.tell(Compute {
                shard,
                reply_to: context.self_ref().clone(),
            });
        }

        Ok(Gathering {
            remaining: SHARDS.len(),
            total: 0,
        })
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(Partial(sum)) => Ok(Control::Continue(Gathering {
                total: state.total + sum,
                ..state
            })),

            // The partial result of the terminated worker has already been added.
            Incoming::Terminated(_) => {
                let remaining = state.remaining - 1;

                if remaining > 0 {
                    Ok(Control::Continue(Gathering { remaining, ..state }))
                } else {
                    println!("## Total is: {}", state.total);
                    Ok(Control::Stop)
                }
            }
        }
    }
}

struct Partial(u64);

struct Gathering {
    remaining: usize,
    total: u64,
}

struct Worker;

impl Actor for Worker {
    type Message = Compute;
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
        let Incoming::Message(Compute { shard, reply_to }) = incoming else {
            unreachable!("worker only receives Compute")
        };

        let (start, end) = (shard.start, shard.end);
        let sum = shard.sum::<u64>();
        println!("## Shard {start}..{end} sums up to: {sum}");
        reply_to.tell(Partial(sum));

        Ok(Control::Stop)
    }
}

struct Compute {
    shard: Range<u64>,
    reply_to: ActorRef<Partial>,
}
