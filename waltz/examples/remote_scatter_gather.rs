//! The scatter-gather example across two nodes: the gatherer scatters its workload to a worker
//! pool on another node and gathers the partial results the workers send back.
//!
//! Both nodes run as separate processes, because a process hosts one remoting endpoint. Running
//! this example starts the gatherer node, which spawns itself again as the worker node, so a
//! single command runs the whole system.
//!
//! The reference to the worker pool is the only one not travelling inside a message: the worker
//! node registers its pool under a key and the gatherer node looks that key up at the address it
//! started the worker node on, so nothing but a name and an address is exchanged. A lookup issued
//! before the worker pool is registered answers `NotFound`, which is why the gatherer retries.
//! Every further reference travels inside messages:
//! `Work::Scatter` carries `reply_to: ActorRef<Partial>` for the gatherer, which the worker pool
//! hands to the workers it spawns locally. Those workers tell their partial results to an actor on
//! another node through the very same `ActorRef` and `tell` they would use locally. Note also that
//! `Compute` needs no serde derives at all: it never crosses the wire, yet it carries a
//! reference to a remote actor.
//!
//! Unlike the local scatter-gather example the gatherer counts the partial results instead of
//! watching its workers: their references never travel to the gatherer, and while remote death
//! watch exists (see docs/remoting.md), a signal synthesized for a dead node cannot prove that a
//! partial result has arrived, so counting replies is what a robust remote gather looks like.
//!
//! The results are printed to stdout and waltz logs to stderr; the log level is configured via
//! `RUST_LOG`, e.g. `RUST_LOG=waltz=debug cargo run --quiet --features remote-dev --example
//! remote_scatter_gather`.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::{
    convert::Infallible,
    env, io,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    ops::Range,
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    time::{sleep, timeout},
};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
use waltz::{
    Actor, ActorContext, ActorRef, ActorSystem, Control, Incoming,
    remote::{self, EndpointConfig, Key, QuicTransport},
};

const SHARDS: [Range<u64>; 4] = [1..26, 26..51, 51..76, 76..101];
const WORKER_NODE_ARG: &str = "worker-node";
const WORKER_POOL_KEY: &str = "worker-pool";
const LOOKUP_INTERVAL: Duration = Duration::from_millis(50);
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    match env::args().nth(1).as_deref() {
        Some(WORKER_NODE_ARG) => worker_node().await,
        _ => gatherer_node().await,
    }
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

async fn worker_node() -> anyhow::Result<()> {
    let bind_addr = env::args().nth(2).context("worker node address argument")?;
    start_endpoint(bind_addr.parse().context("worker node address")?)?;

    let system = ActorSystem::new(WorkerPool);
    remote::register(&Key::new(WORKER_POOL_KEY), system.root())
        .context("registering the worker pool")?;

    system
        .terminated()
        .await
        .context("awaiting actor system termination")
}

struct WorkerPool;

impl Actor for WorkerPool {
    type Message = Work;
    type State = ();
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(())
    }

    fn receive(
        &self,
        context: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        let Incoming::Message(work) = incoming else {
            unreachable!("worker pool only receives Work")
        };

        match work {
            Work::Scatter { shard, reply_to } => {
                let worker = context.spawn(Worker);
                worker.tell(Compute { shard, reply_to });
                Ok(Control::Continue(state))
            }

            Work::Stop => Ok(Control::Stop),
        }
    }
}

#[derive(Serialize, Deserialize)]
enum Work {
    Scatter {
        shard: Range<u64>,
        reply_to: ActorRef<Partial>,
    },

    Stop,
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

async fn gatherer_node() -> anyhow::Result<()> {
    start_endpoint(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;

    let worker_addr = reserved_addr()?;
    let mut worker_node = spawn_worker_node(worker_addr)?;
    let worker_pool = lookup_worker_pool(worker_addr).await?;

    let system = ActorSystem::new(Gatherer { worker_pool });
    system
        .terminated()
        .await
        .context("awaiting actor system termination")?;

    worker_node
        .wait()
        .await
        .context("awaiting the worker node process")?;
    Ok(())
}

struct Gatherer {
    worker_pool: ActorRef<Work>,
}

impl Actor for Gatherer {
    type Message = Partial;
    type State = Gathering;
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        for shard in SHARDS {
            self.worker_pool.tell(Work::Scatter {
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
        let Incoming::Message(Partial(sum)) = incoming else {
            unreachable!("gatherer only receives Partial")
        };

        let total = state.total + sum;

        let remaining = state.remaining - 1;

        if remaining > 0 {
            Ok(Control::Continue(Gathering { remaining, total }))
        } else {
            println!("## Total is: {total}");
            self.worker_pool.tell(Work::Stop);
            Ok(Control::Stop)
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Partial(u64);

struct Gathering {
    remaining: usize,
    total: u64,
}

fn start_endpoint(bind_addr: SocketAddr) -> anyhow::Result<()> {
    let transport = QuicTransport::dev(bind_addr).context("dev QUIC transport")?;
    let advertised_addr = transport.local_addr().context("local transport address")?;
    remote::start(EndpointConfig::new(advertised_addr), transport).context("remoting endpoint")?;
    Ok(())
}

/// A port the OS has just handed out and nothing holds anymore, so the worker node about to be
/// spawned can bind it: the gatherer has to name that address before the node exists.
fn reserved_addr() -> anyhow::Result<SocketAddr> {
    let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .context("reserving a loopback port")?;
    let addr = socket.local_addr().context("reserved local address")?;
    Ok(addr)
}

fn spawn_worker_node(addr: SocketAddr) -> anyhow::Result<Child> {
    Command::new(env::current_exe()?)
        .arg(WORKER_NODE_ARG)
        .arg(addr.to_string())
        .kill_on_drop(true)
        .spawn()
        .context("spawning the worker node process")
}

/// The worker node answers lookups from the moment its endpoint starts, which is a moment before
/// it registers its pool, so `NotFound` is an ordinary bootstrap answer and the one error worth
/// retrying. One deadline bounds the whole loop: the interval only paces the retries, so the two
/// cannot multiply into a second, accidental budget.
async fn lookup_worker_pool(addr: SocketAddr) -> anyhow::Result<ActorRef<Work>> {
    let key = Key::new(WORKER_POOL_KEY);

    let lookup = async {
        loop {
            match remote::lookup(&key, addr).await {
                Ok(worker_pool) => return anyhow::Ok(worker_pool),
                Err(remote::LookupError::NotFound) => sleep(LOOKUP_INTERVAL).await,
                Err(error) => return Err(error).context("resolving the worker pool"),
            }
        }
    };

    timeout(LOOKUP_TIMEOUT, lookup)
        .await
        .with_context(|| format!("no worker pool registered at {addr} within {LOOKUP_TIMEOUT:?}"))?
}
