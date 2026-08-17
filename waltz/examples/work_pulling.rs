//! Work pulling: the manager owns a queue of jobs and each worker requests the next job whenever
//! it is ready, carrying a `ReplyTo` which is single-shot, so one request gets at most one job.
//! A worker thus never has more than one job in flight, which makes a bounded mailbox of capacity
//! one provably sufficient: backpressure by design, without dropping work.
//!
//! The word counts are printed to stdout and waltz logs to stderr; the log level is configured via
//! `RUST_LOG`, e.g. `RUST_LOG=waltz=debug cargo run --quiet -p waltz --example work_pulling`.

use anyhow::Context;
use std::{
    convert::{Infallible, identity},
    io,
    num::NonZeroUsize,
};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
use waltz::{
    Actor, ActorConfig, ActorContext, ActorRef, ActorSystem, Control, Incoming, MailboxCapacity,
    ReplyTo,
};

const DOCUMENTS: [&str; 6] = [
    "the quick brown fox jumps over the lazy dog",
    "actors all the way down",
    "one message at a time",
    "backpressure by design",
    "workers pull instead of being pushed",
    "a terminated signal arrives behind all messages",
];

const WORKERS: usize = 3;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let system = ActorSystem::new(Manager);

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

struct Manager;

impl Actor for Manager {
    type Message = RequestJob;
    type State = Managing;
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        // A worker has at most one job in flight, so a mailbox of capacity one never drops.
        let config = ActorConfig::default()
            .with_mailbox_capacity(MailboxCapacity::Bounded(NonZeroUsize::MIN));

        for _ in 0..WORKERS {
            let worker = context.spawn_with_config(
                Worker {
                    manager: context.self_ref().clone(),
                },
                config,
            );
            context.watch(&worker);
        }

        Ok(Managing {
            jobs: DOCUMENTS.to_vec(),
            workers: WORKERS,
        })
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(RequestJob(reply_to)) => {
                let mut jobs = state.jobs;
                match jobs.pop() {
                    Some(document) => reply_to.reply(Next::Job(document)),
                    None => reply_to.reply(Next::Drained),
                }
                Ok(Control::Continue(Managing { jobs, ..state }))
            }

            Incoming::Terminated(_) => {
                let workers = state.workers - 1;

                if workers > 0 {
                    Ok(Control::Continue(Managing { workers, ..state }))
                } else {
                    println!("## All documents processed");
                    Ok(Control::Stop)
                }
            }
        }
    }
}

struct RequestJob(ReplyTo<Next>);

struct Managing {
    jobs: Vec<&'static str>,
    workers: usize,
}

struct Worker {
    manager: ActorRef<RequestJob>,
}

impl Actor for Worker {
    type Message = Next;
    type State = ();
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        self.request_job(context);
        Ok(())
    }

    fn receive(
        &self,
        context: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        let Incoming::Message(next) = incoming else {
            unreachable!("the worker watches no actor, hence never gets a terminated signal")
        };

        match next {
            Next::Job(document) => {
                let words = document.split_whitespace().count();
                println!("## {words} words in: {document}");

                self.request_job(context);

                Ok(Control::Continue(state))
            }

            Next::Drained => Ok(Control::Stop),
        }
    }
}

impl Worker {
    fn request_job(&self, context: &ActorContext<Next>) {
        // The reply already is this worker's message type, so it converts via `identity`.
        self.manager.tell(RequestJob(context.reply_to(identity)));
    }
}

enum Next {
    Job(&'static str),
    Drained,
}
