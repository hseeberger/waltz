//! The scatter-gather example across two nodes: the gatherer scatters its workload to a worker
//! pool on another node and gathers the partial results the workers send back.
//!
//! Both nodes run as separate processes, because a process hosts one remoting endpoint. Running
//! this example starts the gatherer node, which spawns itself again as the worker node, so a
//! single command runs the whole system.
//!
//! The reference to the worker pool is the only one exchanged out of band: the worker node writes
//! it to a file (via a temporary file it renames, so the gatherer never reads a partial one) and
//! the gatherer node resolves it from there. Every further reference travels inside messages:
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

use anyhow::{Context, bail};
use logforth::{append::Stderr, filter::rustlog::RustLogFilterBuilder, layout::JsonLayout};
use serde::{Deserialize, Serialize};
use std::{
    convert::Infallible,
    env,
    io::ErrorKind,
    net::{Ipv4Addr, SocketAddr},
    ops::Range,
    path::Path,
    process,
    time::Duration,
};
use tokio::{
    fs,
    process::{Child, Command},
    time::sleep,
};
use waltz::{
    Actor, ActorContext, ActorRef, ActorSystem, Control, Incoming,
    remote::{self, EndpointConfig, QuicTransport},
};

const SHARDS: [Range<u64>; 4] = [1..26, 26..51, 51..76, 76..101];
const WORKER_NODE_ARG: &str = "worker-node";
const REF_ATTEMPTS: u32 = 100;
const REF_INTERVAL: Duration = Duration::from_millis(50);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();

    match env::args().nth(1).as_deref() {
        Some(WORKER_NODE_ARG) => worker_node().await,
        _ => gatherer_node().await,
    }
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

async fn worker_node() -> anyhow::Result<()> {
    start_endpoint()?;

    let system = ActorSystem::new(WorkerPool);

    let ref_path = env::args().nth(2).context("reference file path argument")?;
    write_ref(system.root(), Path::new(&ref_path)).await?;

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
    start_endpoint()?;

    let ref_path = env::temp_dir().join(format!("waltz-worker-pool-{}.ref", process::id()));
    let mut worker_node = spawn_worker_node(&ref_path)?;
    let worker_pool = read_worker_pool_ref(&ref_path).await?;

    let system = ActorSystem::new(Gatherer { worker_pool });
    system
        .terminated()
        .await
        .context("awaiting actor system termination")?;

    worker_node
        .wait()
        .await
        .context("awaiting the worker node process")?;
    fs::remove_file(&ref_path)
        .await
        .context("removing the reference file")
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

fn start_endpoint() -> anyhow::Result<()> {
    let transport = QuicTransport::dev(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let advertised_addr = transport.local_addr()?;
    remote::start(EndpointConfig::new(advertised_addr), transport)?;
    Ok(())
}

async fn write_ref(worker_pool: &ActorRef<Work>, path: &Path) -> anyhow::Result<()> {
    let bytes = remote::serialize_ref(worker_pool)?;

    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, bytes)
        .await
        .context("writing the reference file")?;
    fs::rename(&temp_path, path)
        .await
        .context("renaming the reference file")?;

    Ok(())
}

fn spawn_worker_node(ref_path: &Path) -> anyhow::Result<Child> {
    Command::new(env::current_exe()?)
        .arg(WORKER_NODE_ARG)
        .arg(ref_path)
        .kill_on_drop(true)
        .spawn()
        .context("spawning the worker node process")
}

async fn read_worker_pool_ref(path: &Path) -> anyhow::Result<ActorRef<Work>> {
    for _ in 0..REF_ATTEMPTS {
        match fs::read(path).await {
            Ok(bytes) => {
                return remote::deserialize_ref(&bytes)
                    .context("resolving the worker pool reference");
            }

            Err(error) if error.kind() == ErrorKind::NotFound => sleep(REF_INTERVAL).await,

            Err(error) => return Err(error).context("reading the reference file"),
        }
    }

    bail!("no reference file at {}", path.display())
}
