//! Remoting integration: a client node in this process runs scenarios against server nodes in
//! child processes, proving reference serialization, replies through `reply_to`, per-sender FIFO
//! across the wire, and the remote death watch contract: a real termination signals behind all
//! delivered messages, watching an already terminated actor signals immediately, unwatch
//! suppresses the signal, and a killed node yields a synthesized signal via failure detection.

use anyhow::{Context, bail};
use derive_more::{Deref, DerefMut};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    convert::Infallible,
    env,
    io::{BufRead, BufReader, Write},
    net::SocketAddr,
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tokio::{
    runtime::Runtime,
    time::{sleep, timeout},
};
use waltz::{
    Actor, ActorContext, ActorId, ActorRef, ActorSystem, Control, Incoming,
    remote::{self, EndpointConfig, QuicTransport},
};

const ROLE_ENV: &str = "WALTZ_REMOTING_ROLE";
const REF_PREFIX: &str = "REF ";
const PINGS: u32 = 100;
const STREAMED: u32 = 50;
const TIMEOUT: Duration = Duration::from_secs(30);
const UNWATCH_GRACE: Duration = Duration::from_millis(500);
const DEAD_LETTER_GRACE: Duration = Duration::from_millis(300);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

type Receiver = mpsc::Receiver<TestEvent>;

fn main() -> anyhow::Result<()> {
    match env::var(ROLE_ENV).as_deref() {
        Ok("echo") => echo_node(),
        Ok("keeper") => keeper_node(),
        _ => client(),
    }
}

/// The echo node: replies to every ping, stops on `Stop`.
fn echo_node() -> anyhow::Result<()> {
    serve(EchoServer)
}

/// The keeper node: spawns a fresh streamer child per `Spawn` and hands its reference out.
fn keeper_node() -> anyhow::Result<()> {
    serve(Keeper)
}

fn serve<A>(actor: A) -> anyhow::Result<()>
where
    A: Actor + Send + 'static,
    A::Message: DeserializeOwned + Send + 'static,
    A::State: Send + 'static,
{
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        start_endpoint()?;

        let system = ActorSystem::new(actor);
        let bytes = remote::serialize_ref(system.root())?;
        println!("{REF_PREFIX}{}", hex_encode(&bytes));
        std::io::stdout().flush()?;

        timeout(TIMEOUT, system.terminated())
            .await
            .context("server actor system termination")??;
        Ok(())
    })
}

struct EchoServer;

impl Actor for EchoServer {
    type Message = Request;
    type State = ();
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(Request::Ping { seq, reply_to }) => {
                reply_to.tell(Reply { seq });
                Ok(Control::Continue(state))
            }

            Incoming::Message(Request::Stop) => Ok(Control::Stop),

            Incoming::Terminated(_) => Ok(Control::Continue(state)),
        }
    }
}

#[derive(Serialize, Deserialize)]
enum Request {
    Ping { seq: u32, reply_to: ActorRef<Reply> },
    Stop,
}

#[derive(Serialize, Deserialize)]
struct Reply {
    seq: u32,
}

struct Keeper;

impl Actor for Keeper {
    type Message = KeeperMessage;
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
        match incoming {
            Incoming::Message(KeeperMessage::Spawn { reply_to }) => {
                let child = context.spawn(Streamer);
                reply_to.tell(ClientEvent::Child(child));
                Ok(Control::Continue(state))
            }

            Incoming::Message(KeeperMessage::Stop) => Ok(Control::Stop),

            Incoming::Terminated(_) => Ok(Control::Continue(state)),
        }
    }
}

#[derive(Serialize, Deserialize)]
enum KeeperMessage {
    Spawn { reply_to: ActorRef<ClientEvent> },
    Stop,
}

/// Streams `count` numbered messages to `reply_to`, then stops: its terminated signal must
/// arrive behind all of them.
struct Streamer;

impl Actor for Streamer {
    type Message = StreamerMessage;
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
        let Incoming::Message(StreamerMessage::Go { count, reply_to }) = incoming else {
            unreachable!("streamer only receives Go")
        };

        for seq in 0..count {
            reply_to.tell(ClientEvent::Streamed(seq));
        }
        Ok(Control::Stop)
    }
}

#[derive(Serialize, Deserialize)]
enum StreamerMessage {
    Go {
        count: u32,
        reply_to: ActorRef<ClientEvent>,
    },
}

#[derive(Serialize, Deserialize)]
enum ClientEvent {
    Child(ActorRef<StreamerMessage>),
    Streamed(u32),
    Probe,
}

enum TestEvent {
    Watching,
    Child(ActorRef<StreamerMessage>),
    StreamedAll,
    Done(Result<(), String>),
}

fn client() -> anyhow::Result<()> {
    let runtime = Runtime::new()?;
    let _guard = runtime.enter();
    start_endpoint()?;

    echo_scenario(&runtime).context("echo scenario")?;

    let mut keeper_process = KillOnDrop(spawn_node("keeper")?);
    let keeper = resolve_ref::<KeeperMessage>(&mut keeper_process)?;

    ordered_termination_scenario(&runtime, &keeper).context("ordered termination scenario")?;
    watch_terminated_scenario(&runtime, &keeper).context("watch terminated scenario")?;
    unwatch_scenario(&runtime, &keeper).context("unwatch scenario")?;

    keeper.tell(KeeperMessage::Stop);
    let status = wait_with_timeout(&mut keeper_process, TIMEOUT)?;
    if !status.success() {
        bail!("keeper process exited with {status}");
    }

    node_death_scenario(&runtime).context("node death scenario")?;

    Ok(())
}

/// Round trip and per-sender FIFO: ordered pings, ordered replies. Then the node is killed and
/// told anyway, and a second node is served afterwards: only that further round trip proves the
/// dead letters left the endpoint usable, which the sends alone cannot show.
fn echo_scenario(runtime: &Runtime) -> anyhow::Result<()> {
    let mut echo_process = KillOnDrop(spawn_node("echo")?);
    let echo = resolve_ref::<Request>(&mut echo_process)?;

    let event_rx = run_client(runtime, "echo client termination", |event_tx| EchoClient {
        server: echo.clone(),
        event_tx,
    })?;
    expect_done(&event_rx)?;

    let status = wait_with_timeout(&mut echo_process, TIMEOUT)?;
    if !status.success() {
        bail!("echo process exited with {status}");
    }

    runtime.block_on(async {
        echo.tell(Request::Stop);
        sleep(DEAD_LETTER_GRACE).await;
        echo.tell(Request::Stop);
    });

    let mut echo_process = KillOnDrop(spawn_node("echo")?);
    let echo = resolve_ref::<Request>(&mut echo_process)?;

    let event_rx = run_client(
        runtime,
        "echo client termination after dead letters",
        |event_tx| EchoClient {
            server: echo,
            event_tx,
        },
    )?;
    expect_done(&event_rx)?;

    let status = wait_with_timeout(&mut echo_process, TIMEOUT)?;
    if !status.success() {
        bail!("echo process exited with {status}");
    }
    Ok(())
}

/// A watched remote actor streams `STREAMED` messages and stops: the terminated signal must
/// arrive behind all of them, in order.
fn ordered_termination_scenario(
    runtime: &Runtime,
    keeper: &ActorRef<KeeperMessage>,
) -> anyhow::Result<()> {
    let event_rx = run_client(runtime, "ordered watch client termination", |event_tx| {
        OrderedWatch {
            keeper: keeper.clone(),
            event_tx,
        }
    })?;

    let TestEvent::Child(_) = recv(&event_rx)? else {
        bail!("no child reference from the ordered watch client");
    };
    expect_done(&event_rx)
}

/// Watching an already terminated remote actor must still deliver the terminated signal, and
/// must do so via the watched node's immediate answer, since the node is alive and heartbeating.
/// The subject is terminated by this scenario itself, so the precondition is asserted where it is
/// relied upon rather than inherited from another scenario.
fn watch_terminated_scenario(
    runtime: &Runtime,
    keeper: &ActorRef<KeeperMessage>,
) -> anyhow::Result<()> {
    let dead_child = terminated_child(runtime, keeper).context("terminated child")?;

    let event_rx = run_client(runtime, "watch terminated client termination", |event_tx| {
        WatchTerminated {
            child: dead_child,
            event_tx,
        }
    })?;
    expect_done(&event_rx)
}

/// A reference to a remote actor which has provably terminated: it is only returned after its
/// terminated signal has been received.
fn terminated_child(
    runtime: &Runtime,
    keeper: &ActorRef<KeeperMessage>,
) -> anyhow::Result<ActorRef<StreamerMessage>> {
    let event_rx = run_client(runtime, "terminate child client termination", |event_tx| {
        TerminateChild {
            keeper: keeper.clone(),
            event_tx,
        }
    })?;

    let TestEvent::Child(child) = recv(&event_rx)? else {
        bail!("no terminated child reference");
    };
    expect_done(&event_rx)?;
    Ok(child)
}

/// After unwatch no terminated signal may be received, even though the remote actor terminates.
fn unwatch_scenario(runtime: &Runtime, keeper: &ActorRef<KeeperMessage>) -> anyhow::Result<()> {
    let (event_tx, event_rx) = mpsc::channel();
    let system = ActorSystem::new(UnwatchClient {
        keeper: keeper.clone(),
        event_tx,
    });

    match recv(&event_rx)? {
        TestEvent::StreamedAll => {}
        TestEvent::Done(result) => bail!("done before probe: {result:?}"),
        _ => bail!("unexpected event from the unwatch client"),
    }
    thread::sleep(UNWATCH_GRACE);
    system.root().tell(ClientEvent::Probe);

    runtime
        .block_on(timeout(TIMEOUT, system.terminated()))
        .context("unwatch client termination")??;
    expect_done(&event_rx)
}

/// Killing the watched actor's node must yield a synthesized terminated signal via failure
/// detection.
fn node_death_scenario(runtime: &Runtime) -> anyhow::Result<()> {
    let mut keeper_process = KillOnDrop(spawn_node("keeper")?);
    let keeper = resolve_ref::<KeeperMessage>(&mut keeper_process)?;

    let (event_tx, event_rx) = mpsc::channel();
    let system = ActorSystem::new(NodeDeathWatch { keeper, event_tx });

    match recv(&event_rx)? {
        TestEvent::Watching => {}
        _ => bail!("unexpected event from the node death client"),
    }
    keeper_process
        .0
        .kill()
        .context("killing the keeper process")?;

    expect_done(&event_rx)?;
    runtime
        .block_on(timeout(TIMEOUT, system.terminated()))
        .context("node death client termination")??;
    Ok(())
}

struct EchoClient {
    server: ActorRef<Request>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for EchoClient {
    type Message = Reply;
    type State = u32;
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        for seq in 0..PINGS {
            self.server.tell(Request::Ping {
                seq,
                reply_to: context.self_ref().clone(),
            });
        }
        Ok(0)
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        expected: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        let Incoming::Message(Reply { seq }) = incoming else {
            return Ok(Control::Continue(expected));
        };

        if seq != expected {
            let _ = self.event_tx.send(TestEvent::Done(Err(format!(
                "reply {seq} instead of {expected}"
            ))));
            return Ok(Control::Stop);
        }

        let next = expected + 1;
        if next == PINGS {
            let _ = self.event_tx.send(TestEvent::Done(Ok(())));
            self.server.tell(Request::Stop);
            Ok(Control::Stop)
        } else {
            Ok(Control::Continue(next))
        }
    }
}

struct OrderedWatch {
    keeper: ActorRef<KeeperMessage>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for OrderedWatch {
    type Message = ClientEvent;
    type State = OrderedWatchState;
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        self.keeper.tell(KeeperMessage::Spawn {
            reply_to: context.self_ref().clone(),
        });
        Ok(OrderedWatchState::AwaitingChild)
    }

    fn receive(
        &self,
        context: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        let fail = |message: String| {
            let _ = self.event_tx.send(TestEvent::Done(Err(message)));
            Ok(Control::Stop)
        };

        match (incoming, state) {
            (Incoming::Message(ClientEvent::Child(child)), OrderedWatchState::AwaitingChild) => {
                context.watch(&child);
                child.tell(StreamerMessage::Go {
                    count: STREAMED,
                    reply_to: context.self_ref().clone(),
                });
                let child_id = child.actor_id();
                let _ = self.event_tx.send(TestEvent::Child(child));
                Ok(Control::Continue(OrderedWatchState::Streaming {
                    child_id,
                    next: 0,
                }))
            }

            (
                Incoming::Message(ClientEvent::Streamed(seq)),
                OrderedWatchState::Streaming { child_id, next },
            ) => {
                if seq != next {
                    return fail(format!("streamed {seq} instead of {next}"));
                }
                Ok(Control::Continue(OrderedWatchState::Streaming {
                    child_id,
                    next: next + 1,
                }))
            }

            (Incoming::Terminated(id), OrderedWatchState::Streaming { child_id, next }) => {
                if id != child_id {
                    return fail(format!("terminated signal for unexpected actor {id}"));
                }
                if next != STREAMED {
                    return fail(format!(
                        "terminated signal after {next} of {STREAMED} messages"
                    ));
                }
                let _ = self.event_tx.send(TestEvent::Done(Ok(())));
                Ok(Control::Stop)
            }

            _ => fail("unexpected incoming".to_string()),
        }
    }
}

/// Spawns a streamer on the keeper's node, watches it, stops it and only reports its reference
/// once the terminated signal has arrived.
struct TerminateChild {
    keeper: ActorRef<KeeperMessage>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for TerminateChild {
    type Message = ClientEvent;
    type State = Option<ActorRef<StreamerMessage>>;
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        self.keeper.tell(KeeperMessage::Spawn {
            reply_to: context.self_ref().clone(),
        });
        Ok(None)
    }

    fn receive(
        &self,
        context: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(ClientEvent::Child(child)) => {
                context.watch(&child);
                child.tell(StreamerMessage::Go {
                    count: 0,
                    reply_to: context.self_ref().clone(),
                });
                Ok(Control::Continue(Some(child)))
            }

            Incoming::Terminated(id) => {
                let result = match &state {
                    Some(child) if child.actor_id() == id => Ok(()),
                    _ => Err(format!("terminated signal for unexpected actor {id}")),
                };

                if let Some(child) = state {
                    let _ = self.event_tx.send(TestEvent::Child(child));
                }
                let _ = self.event_tx.send(TestEvent::Done(result));
                Ok(Control::Stop)
            }

            _ => Ok(Control::Continue(state)),
        }
    }
}

enum OrderedWatchState {
    AwaitingChild,
    Streaming { child_id: ActorId, next: u32 },
}

struct WatchTerminated {
    child: ActorRef<StreamerMessage>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for WatchTerminated {
    type Message = ClientEvent;
    type State = ();
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        context.watch(&self.child);
        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        let Incoming::Terminated(id) = incoming else {
            return Ok(Control::Continue(state));
        };

        let result = if id == self.child.actor_id() {
            Ok(())
        } else {
            Err(format!("terminated signal for unexpected actor {id}"))
        };
        let _ = self.event_tx.send(TestEvent::Done(result));
        Ok(Control::Stop)
    }
}

struct UnwatchClient {
    keeper: ActorRef<KeeperMessage>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for UnwatchClient {
    type Message = ClientEvent;
    type State = u32;
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        self.keeper.tell(KeeperMessage::Spawn {
            reply_to: context.self_ref().clone(),
        });
        Ok(0)
    }

    fn receive(
        &self,
        context: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        received: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(ClientEvent::Child(child)) => {
                context.watch(&child);
                context.unwatch(&child);
                child.tell(StreamerMessage::Go {
                    count: STREAMED,
                    reply_to: context.self_ref().clone(),
                });
                Ok(Control::Continue(received))
            }

            Incoming::Message(ClientEvent::Streamed(_)) => {
                let received = received + 1;
                if received == STREAMED {
                    let _ = self.event_tx.send(TestEvent::StreamedAll);
                }
                Ok(Control::Continue(received))
            }

            Incoming::Message(ClientEvent::Probe) => {
                let _ = self.event_tx.send(TestEvent::Done(Ok(())));
                Ok(Control::Stop)
            }

            Incoming::Terminated(id) => {
                let _ = self.event_tx.send(TestEvent::Done(Err(format!(
                    "terminated signal for {id} despite unwatch"
                ))));
                Ok(Control::Stop)
            }
        }
    }
}

struct NodeDeathWatch {
    keeper: ActorRef<KeeperMessage>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for NodeDeathWatch {
    type Message = ClientEvent;
    type State = ();
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        context.watch(&self.keeper);
        let _ = self.event_tx.send(TestEvent::Watching);
        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        let Incoming::Terminated(id) = incoming else {
            return Ok(Control::Continue(state));
        };

        let result = if id == self.keeper.actor_id() {
            Ok(())
        } else {
            Err(format!("terminated signal for unexpected actor {id}"))
        };
        let _ = self.event_tx.send(TestEvent::Done(result));
        Ok(Control::Stop)
    }
}

/// Run a client actor to completion and hand back the events it reported.
fn run_client<A, F>(runtime: &Runtime, what: &'static str, actor: F) -> anyhow::Result<Receiver>
where
    A: Actor + Send + 'static,
    A::Message: Send + 'static,
    A::State: Send + 'static,
    F: FnOnce(mpsc::Sender<TestEvent>) -> A,
{
    let (event_tx, event_rx) = mpsc::channel();
    let system = ActorSystem::new(actor(event_tx));

    runtime
        .block_on(timeout(TIMEOUT, system.terminated()))
        .context(what)??;
    Ok(event_rx)
}

/// Start this process's remoting endpoint on an OS chosen port; must run inside a Tokio runtime.
fn start_endpoint() -> anyhow::Result<SocketAddr> {
    let transport = QuicTransport::dev("127.0.0.1:0".parse()?)?;
    let addr = transport.local_addr()?;
    remote::start(EndpointConfig::new(addr), transport)?;
    Ok(addr)
}

fn spawn_node(role: &str) -> anyhow::Result<Child> {
    let child = Command::new(env::current_exe()?)
        .env(ROLE_ENV, role)
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("{role} process"))?;
    Ok(child)
}

fn resolve_ref<M>(child: &mut Child) -> anyhow::Result<ActorRef<M>>
where
    M: Serialize + Send + 'static,
{
    let stdout = child.stdout.take().context("server process stdout")?;
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if let Some(hex) = line.strip_prefix(REF_PREFIX) {
            let bytes = hex_decode(hex)?;
            return remote::deserialize_ref(&bytes).context("server reference");
        }
    }
    bail!("no server reference on the server process stdout");
}

fn recv(event_rx: &mpsc::Receiver<TestEvent>) -> anyhow::Result<TestEvent> {
    event_rx.recv_timeout(TIMEOUT).context("test event")
}

fn expect_done(event_rx: &mpsc::Receiver<TestEvent>) -> anyhow::Result<()> {
    match recv(event_rx)? {
        TestEvent::Done(Ok(())) => Ok(()),
        TestEvent::Done(Err(message)) => bail!(message),
        _ => bail!("unexpected event instead of done"),
    }
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> anyhow::Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            bail!("server process still running after {timeout:?}");
        }
        thread::sleep(EXIT_POLL_INTERVAL);
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_decode(hex: &str) -> anyhow::Result<Vec<u8>> {
    let pairs = hex.as_bytes().chunks_exact(2);
    if !pairs.remainder().is_empty() {
        bail!("odd length hex encoded reference");
    }

    pairs
        .map(|pair| {
            let pair = str::from_utf8(pair).context("hex encoded reference")?;
            u8::from_str_radix(pair, 16).context("hex encoded reference")
        })
        .collect()
}

#[derive(Deref, DerefMut)]
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}
