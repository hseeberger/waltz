//! Messaging throughput benchmarks, reported as messages per second:
//!
//! - `flood`: the bench thread floods a single counting actor, with an unbounded mailbox and with a
//!   bounded one whose capacity is large enough that no message is ever dropped.
//! - `ping_pong`: pairs of actors play ping-pong, one pair and one pair per core.
//! - `fan_out`: the root actor sends messages round-robin to its workers, one worker per core and
//!   four workers per core.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::{
    convert::Infallible,
    num::NonZeroUsize,
    thread,
    time::{Duration, Instant},
};
use tokio::runtime::Runtime;
use waltz::{
    Actor, ActorConfig, ActorContext, ActorRef, ActorSystem, Control, Incoming, MailboxCapacity,
};

const FLOOD_MESSAGES: usize = 100_000;
const FLOOD_CAPACITY: NonZeroUsize =
    NonZeroUsize::new(FLOOD_MESSAGES).expect("flood message count is not zero");
const PING_PONG_ROUNDS: usize = 1_000;
const FAN_OUT_MESSAGES: usize = 100_000;

fn flood(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime can be created");

    let mut group = c.benchmark_group("flood");
    group.throughput(Throughput::Elements(FLOOD_MESSAGES as u64));

    let mailbox_capacities = [
        ("unbounded", MailboxCapacity::Unbounded),
        ("bounded", MailboxCapacity::Bounded(FLOOD_CAPACITY)),
    ];
    for (label, mailbox_capacity) in mailbox_capacities {
        group.bench_function(label, |b| {
            b.to_async(&rt).iter_custom(|iters| async move {
                measure(
                    iters,
                    || {
                        let actor = Countdown {
                            messages: FLOOD_MESSAGES,
                        };
                        let config = ActorConfig::default().with_mailbox_capacity(mailbox_capacity);
                        (actor, config)
                    },
                    |root| {
                        for _ in 0..FLOOD_MESSAGES {
                            root.tell(Tick);
                        }
                    },
                )
                .await
            });
        });
    }

    group.finish();
}

fn ping_pong(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime can be created");

    let mut group = c.benchmark_group("ping_pong");

    for pairs in [1, available_parallelism()] {
        group.throughput(Throughput::Elements((pairs * PING_PONG_ROUNDS * 2) as u64));

        group.bench_function(BenchmarkId::new("pairs", pairs), |b| {
            b.to_async(&rt).iter_custom(|iters| async move {
                measure(
                    iters,
                    || {
                        let actor = PingPongRoot {
                            pairs,
                            rounds: PING_PONG_ROUNDS,
                        };
                        (actor, ActorConfig::default())
                    },
                    |root| root.tell(Go),
                )
                .await
            });
        });
    }

    group.finish();
}

fn fan_out(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime can be created");

    let mut group = c.benchmark_group("fan_out");

    for workers in [available_parallelism(), 4 * available_parallelism()] {
        let share = FAN_OUT_MESSAGES / workers;
        group.throughput(Throughput::Elements((share * workers) as u64));

        group.bench_function(BenchmarkId::new("workers", workers), |b| {
            b.to_async(&rt).iter_custom(|iters| async move {
                measure(
                    iters,
                    || (FanOutRoot { workers, share }, ActorConfig::default()),
                    |root| root.tell(Go),
                )
                .await
            });
        });
    }

    group.finish();
}

async fn measure<A, F, D>(iters: u64, make: F, drive: D) -> Duration
where
    A: Actor + Send + 'static,
    A::Message: Send + 'static,
    A::State: Send + 'static,
    F: Fn() -> (A, ActorConfig),
    D: Fn(&ActorRef<A::Message>),
{
    let mut elapsed = Duration::ZERO;

    for _ in 0..iters {
        let (actor, config) = make();
        let system = ActorSystem::with_config(actor, config);

        let start = Instant::now();
        drive(system.root());
        system
            .terminated()
            .await
            .expect("awaiting actor system termination");
        elapsed += start.elapsed();
    }

    elapsed
}

fn available_parallelism() -> usize {
    thread::available_parallelism()
        .expect("available parallelism can be determined")
        .get()
}

fn next_remaining(remaining: usize) -> Option<usize> {
    remaining.checked_sub(1).filter(|n| *n > 0)
}

fn next_control(remaining: usize) -> Control<usize> {
    next_remaining(remaining).map_or(Control::Stop, Control::Continue)
}

struct Tick;

struct Countdown {
    messages: usize,
}

impl Actor for Countdown {
    type Message = Tick;
    type State = usize;
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(self.messages)
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        _: Incoming<Self::Message>,
        remaining: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        Ok(next_control(remaining))
    }
}

struct Go;

struct PingPongRoot {
    pairs: usize,
    rounds: usize,
}

impl Actor for PingPongRoot {
    type Message = Go;
    type State = usize;
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(self.pairs)
    }

    fn receive(
        &self,
        context: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        remaining: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(Go) => {
                for _ in 0..self.pairs {
                    let pinger = context.spawn(Pinger {
                        rounds: self.rounds,
                    });
                    context.watch(&pinger);
                }

                Ok(Control::Continue(remaining))
            }

            Incoming::Terminated(_) => Ok(next_control(remaining)),
        }
    }
}

struct Pong;

struct Pinger {
    rounds: usize,
}

impl Actor for Pinger {
    type Message = Pong;
    type State = Pinging;
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        let ponger = context.spawn(Ponger {
            pinger: context.self_ref().clone(),
        });
        ponger.tell(Ping);

        Ok(Pinging {
            ponger,
            remaining: self.rounds,
        })
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        _: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match next_remaining(state.remaining) {
            Some(remaining) => {
                state.ponger.tell(Ping);
                Ok(Control::Continue(Pinging { remaining, ..state }))
            }

            None => Ok(Control::Stop),
        }
    }
}

struct Pinging {
    ponger: ActorRef<Ping>,
    remaining: usize,
}

struct Ping;

struct Ponger {
    pinger: ActorRef<Pong>,
}

impl Actor for Ponger {
    type Message = Ping;
    type State = ();
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        _: Incoming<Self::Message>,
        _: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        self.pinger.tell(Pong);

        Ok(Control::Continue(()))
    }
}

struct FanOutRoot {
    workers: usize,
    share: usize,
}

impl Actor for FanOutRoot {
    type Message = Go;
    type State = FanningOut;
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        let workers = (0..self.workers)
            .map(|_| {
                let worker = context.spawn(Countdown {
                    messages: self.share,
                });
                context.watch(&worker);
                worker
            })
            .collect::<Vec<_>>();

        Ok(FanningOut {
            workers,
            remaining: self.workers,
        })
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(Go) => {
                for worker in state
                    .workers
                    .iter()
                    .cycle()
                    .take(self.share * state.workers.len())
                {
                    worker.tell(Tick);
                }

                Ok(Control::Continue(state))
            }

            Incoming::Terminated(_) => Ok(next_remaining(state.remaining)
                .map_or(Control::Stop, |remaining| {
                    Control::Continue(FanningOut { remaining, ..state })
                })),
        }
    }
}

struct FanningOut {
    workers: Vec<ActorRef<Tick>>,
    remaining: usize,
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .noise_threshold(0.05)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(5));
    targets = flood, ping_pong, fan_out
);
criterion_main!(benches);
