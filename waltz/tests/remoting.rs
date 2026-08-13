//! Remoting integration: a client node in this process runs scenarios against server nodes in
//! child processes, proving reference serialization, discovery by name and address, replies
//! through `reply_to`, per-sender FIFO across the wire, per-target streams (a bulk message towards
//! one actor does not delay messages towards others), and the remote death watch contract: a real
//! termination signals behind all delivered messages, every watcher of one actor gets its own
//! signal, watching an already terminated actor signals immediately, unwatch suppresses the
//! signal, and a killed node yields a synthesized signal via failure detection. The reconnect
//! path is covered by an oversize message dead-lettering locally while its lane stays usable, by
//! a mid-stream sever proving per-sender FIFO stays "in order, with gaps" over the reconnected
//! lane, by a terminated frame dropped on the watched node being healed by the watch refresh, and
//! by a node killed under a watch and restarted at its old address: the tombstone kills the old
//! incarnation, not the address, and discovery plus FIFO work against the new one. Request-
//! response crosses nodes through the serializable `ReplyTo`: a remote `ask` resolves with the
//! reply, a responder dropping its `ReplyTo` resolves the ask as `NoReply` rather than by
//! timeout, a request beyond `max_frame_size` fails its ask at the send and an oversize reply
//! resolves it as `NoReply`, both instead of by timeout, a reply stays FIFO with the responder's
//! other messages to the asker, a forwarded
//! `ReplyTo` chains its reply over two hops, a `ReplyTo` serialized and resolved on its own node
//! comes home (and refuses a second serialization), killing a node holding a `ReplyTo` fails
//! the pending ask as `NoReply` via failure detection, an ask towards a terminated remote actor
//! resolves as `NoReply` through the frame's reply tags, and killing an unwatched node holding a
//! `ReplyTo` fails the pending ask as `NoReply` once its lane is given up.

use anyhow::{Context, bail};
use derive_more::{Deref, DerefMut};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    convert::Infallible,
    env,
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, UdpSocket},
    num::NonZeroUsize,
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
    Actor, ActorContext, ActorId, ActorRef, ActorSystem, AskError, Control, Incoming, ReplyTo,
    remote::{
        self, ConnectedControl, EndpointConfig, Key, QuicConnection, QuicTransport, Transport,
        TransportError,
    },
};

const ROLE_ENV: &str = "WALTZ_REMOTING_ROLE";
const ADDR_ENV: &str = "WALTZ_REMOTING_ADDR";
const REF_PREFIX: &str = "REF ";
const ECHO_KEY: &str = "echo";
const PINGS: u32 = 100;
const STREAMED: u32 = 50;
const WATCHERS: usize = 2;
const BULK_TARGETS: usize = 8;
const BULKS: u32 = 8;
const BULK_ACKNOWLEDGEMENTS: u32 = BULKS + BULK_TARGETS as u32 - 1;
const BULK_PAYLOAD: usize = 512 * 1_024;
const OVERSIZE_PAYLOAD: usize = 2 * 1_024 * 1_024;
const SEVER_BULKS: u32 = 300;
const SEVER_PAYLOAD: usize = 32 * 1_024;
const SEVER_DELAY: Duration = Duration::from_millis(10);
const MARKER_ATTEMPTS: usize = 10;
const MARKER_RETRY_DELAY: Duration = Duration::from_millis(500);
const RESTART_LOOKUP_ATTEMPTS: usize = 20;
const RESTART_LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);
const RESTART_LOOKUP_DELAY: Duration = Duration::from_millis(250);
const ASKS: u32 = 10;
const FORWARD_SEQ: u32 = 11;
const ROUND_TRIP_SEQ: u32 = 7;
const TIMEOUT: Duration = Duration::from_secs(30);
const GIVE_UP_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const UNWATCH_GRACE: Duration = Duration::from_millis(500);
const DEAD_LETTER_GRACE: Duration = Duration::from_millis(300);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const BENCH_PINGS: u32 = 50_000;
const BENCH_WINDOW: u32 = 4_096;
const BENCH_SERIAL: u32 = 2_000;
const BENCH_BULKS: u32 = 2_000;
const BENCH_BULK_WINDOW: u32 = 64;
const BENCH_BULK_PAYLOAD: usize = 64 * 1_024;

type Receiver = mpsc::Receiver<TestEvent>;

fn main() -> anyhow::Result<()> {
    match env::var(ROLE_ENV).as_deref() {
        Ok(role) if role == Role::Echo.as_str() => echo_node(),
        Ok(role) if role == Role::Keeper.as_str() => keeper_node(),
        Ok(role) if role == Role::Bench.as_str() => bench_client(),
        Ok(role) => bail!("unknown role {role}"),
        Err(_) => client(),
    }
}

/// The role a spawned copy of this process plays; the closed set keeps a spawn site and the
/// dispatch in `main` from drifting apart, where a typo would spawn a second client.
#[derive(Debug, Clone, Copy)]
enum Role {
    Echo,
    Keeper,
    Bench,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Echo => "echo",
            Role::Keeper => "keeper",
            Role::Bench => "bench",
        }
    }
}

/// The echo node: replies to every ping, stops on `Stop`.
fn echo_node() -> anyhow::Result<()> {
    serve(EchoServer, ECHO_KEY)
}

/// The keeper node: spawns a fresh streamer child per `Spawn` and hands its reference out.
fn keeper_node() -> anyhow::Result<()> {
    serve(Keeper, "keeper")
}

/// Serves its root both ways: registered under a key for discovery and printed as a serialized
/// reference, so the scenarios can bootstrap either way against the same node.
fn serve<A>(actor: A, name: &str) -> anyhow::Result<()>
where
    A: Actor + Send + 'static,
    A::Message: DeserializeOwned + Send + 'static,
    A::State: Send + 'static,
{
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        start_endpoint()?;

        let system = ActorSystem::new(actor);
        remote::register(&Key::new(name), system.root())?;
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
    type State = Vec<ReplyTo<Reply>>;
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(Vec::new())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        mut state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(Request::Ping { seq, reply_to }) => {
                reply_to.tell(Reply { seq });
                Ok(Control::Continue(state))
            }

            Incoming::Message(Request::Ask { seq, reply_to }) => {
                reply_to.reply(Reply { seq });
                Ok(Control::Continue(state))
            }

            Incoming::Message(Request::AskThenTell {
                marker_to,
                reply_to,
            }) => {
                marker_to.tell(AskerMessage::Marker);
                reply_to.reply(Reply { seq: 0 });
                Ok(Control::Continue(state))
            }

            Incoming::Message(Request::Ignore { .. }) => Ok(Control::Continue(state)),

            Incoming::Message(Request::AskOversize { reply_to, .. }) => {
                reply_to.reply(Reply { seq: 0 });
                Ok(Control::Continue(state))
            }

            Incoming::Message(Request::AskOversizeReply { reply_to }) => {
                reply_to.reply(BulkReply {
                    payload: vec![0; OVERSIZE_PAYLOAD],
                });
                Ok(Control::Continue(state))
            }

            Incoming::Message(Request::Hold { reply_to }) => {
                state.push(reply_to);
                Ok(Control::Continue(state))
            }

            Incoming::Message(Request::Forward { to, reply_to }) => {
                to.tell(ForwardedRequest { reply_to });
                Ok(Control::Continue(state))
            }

            Incoming::Message(Request::Stop) => Ok(Control::Stop),

            Incoming::Terminated(_) => Ok(Control::Continue(state)),
        }
    }
}

#[derive(Serialize, Deserialize)]
enum Request {
    Ping {
        seq: u32,
        reply_to: ActorRef<Reply>,
    },

    Ask {
        seq: u32,
        reply_to: ReplyTo<Reply>,
    },

    AskThenTell {
        marker_to: ActorRef<AskerMessage>,
        reply_to: ReplyTo<Reply>,
    },

    Ignore {
        reply_to: ReplyTo<Reply>,
    },

    AskOversize {
        payload: Vec<u8>,
        reply_to: ReplyTo<Reply>,
    },

    AskOversizeReply {
        reply_to: ReplyTo<BulkReply>,
    },

    Hold {
        reply_to: ReplyTo<Reply>,
    },

    Forward {
        to: ActorRef<ForwardedRequest>,
        reply_to: ReplyTo<Reply>,
    },

    Stop,
}

#[derive(Debug, Serialize, Deserialize)]
struct Reply {
    seq: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct BulkReply {
    payload: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
enum AskerMessage {
    Marker,
    Answer(Reply),
}

#[derive(Serialize, Deserialize)]
struct ForwardedRequest {
    reply_to: ReplyTo<Reply>,
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

            Incoming::Message(KeeperMessage::DropTerminated { count, reply_to }) => {
                assert!(
                    remote::drop_terminated_frames(count),
                    "endpoint not started"
                );
                reply_to.tell(ClientEvent::Armed);
                Ok(Control::Continue(state))
            }

            Incoming::Message(KeeperMessage::Stop) => Ok(Control::Stop),

            Incoming::Terminated(_) => Ok(Control::Continue(state)),
        }
    }
}

#[derive(Serialize, Deserialize)]
enum KeeperMessage {
    Spawn {
        reply_to: ActorRef<ClientEvent>,
    },

    DropTerminated {
        count: u64,
        reply_to: ActorRef<ClientEvent>,
    },

    Stop,
}

/// Streams `count` numbered messages to `reply_to` and stops on `Go`, so its terminated signal
/// must arrive behind all of them; acknowledges a `Bulk` with its sequence number and keeps
/// running, so a payload's size only ever delays the stream it rides.
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
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(StreamerMessage::Go { count, reply_to }) => {
                for seq in 0..count {
                    reply_to.tell(ClientEvent::Streamed(seq));
                }
                Ok(Control::Stop)
            }

            Incoming::Message(StreamerMessage::Bulk { seq, reply_to, .. }) => {
                reply_to.tell(ClientEvent::Bulked(seq));
                Ok(Control::Continue(state))
            }

            Incoming::Message(StreamerMessage::Ask { reply_to }) => {
                reply_to.reply(Reply { seq: 0 });
                Ok(Control::Continue(state))
            }

            Incoming::Terminated(_) => Ok(Control::Continue(state)),
        }
    }
}

#[derive(Serialize, Deserialize)]
enum StreamerMessage {
    Go {
        count: u32,
        reply_to: ActorRef<ClientEvent>,
    },

    Bulk {
        seq: u32,
        payload: Vec<u8>,
        reply_to: ActorRef<ClientEvent>,
    },

    Ask {
        reply_to: ReplyTo<Reply>,
    },
}

#[derive(Serialize, Deserialize)]
enum ClientEvent {
    Child(ActorRef<StreamerMessage>),
    Streamed(u32),
    Bulked(u32),
    Armed,
    Probe,
}

enum TestEvent {
    Watching,
    Bulking,
    Child(ActorRef<StreamerMessage>),
    StreamedAll,
    Done(Result<(), String>),
}

fn client() -> anyhow::Result<()> {
    let runtime = Runtime::new()?;
    let _guard = runtime.enter();
    start_endpoint()?;

    echo_scenario(&runtime).context("echo scenario")?;
    discovery_scenario(&runtime).context("discovery scenario")?;

    let mut keeper_process = KillOnDrop(spawn_node(Role::Keeper)?);
    let keeper = resolve_ref::<KeeperMessage>(&mut keeper_process)?;

    ordered_termination_scenario(&runtime, &keeper).context("ordered termination scenario")?;
    watch_terminated_scenario(&runtime, &keeper).context("watch terminated scenario")?;
    unwatch_scenario(&runtime, &keeper).context("unwatch scenario")?;
    two_watchers_scenario(&runtime, &keeper).context("two watchers scenario")?;
    head_of_line_scenario(&runtime, &keeper).context("head of line scenario")?;
    oversize_scenario(&runtime, &keeper).context("oversize scenario")?;
    sever_scenario(&runtime, &keeper).context("sever scenario")?;
    lost_terminated_scenario(&runtime, &keeper).context("lost terminated scenario")?;
    dead_target_ask_scenario(&runtime, &keeper).context("dead target ask scenario")?;

    keeper.tell(KeeperMessage::Stop);
    expect_exit(&mut keeper_process, "keeper")?;

    let mut ask_echo_process = KillOnDrop(spawn_node(Role::Echo)?);
    let ask_echo = resolve_ref::<Request>(&mut ask_echo_process)?;

    remote_ask_scenario(&runtime, &ask_echo).context("remote ask scenario")?;
    reply_to_fifo_scenario(&runtime, &ask_echo).context("reply to fifo scenario")?;
    forwarded_reply_scenario(&runtime, &ask_echo).context("forwarded reply scenario")?;
    reply_serde_scenario(&runtime).context("reply serde scenario")?;

    ask_echo.tell(Request::Stop);
    expect_exit(&mut ask_echo_process, "ask echo")?;

    ask_node_death_scenario(&runtime).context("ask node death scenario")?;
    ask_give_up_scenario(&runtime).context("ask give up scenario")?;

    node_death_scenario(&runtime).context("node death scenario")?;
    restart_scenario(&runtime).context("restart scenario")?;

    Ok(())
}

/// Measures the remote hot paths against fresh echo and keeper nodes and prints the numbers:
/// windowed pipelined round trips, serial round trip latency and bulk payload throughput. Not a
/// regression gate; run via `just bench-remote` and compare before and after a change.
fn bench_client() -> anyhow::Result<()> {
    let runtime = Runtime::new()?;
    let _guard = runtime.enter();
    start_endpoint()?;

    let mut echo_process = KillOnDrop(spawn_node(Role::Echo)?);
    let echo = resolve_ref::<Request>(&mut echo_process)?;

    let pipelined = bench_echo(&runtime, &echo, BENCH_PINGS, BENCH_WINDOW)?;
    println!(
        "pipelined: {:.0} round trips/s ({BENCH_PINGS} pings, window {BENCH_WINDOW})",
        f64::from(BENCH_PINGS) / pipelined.as_secs_f64()
    );

    let serial = bench_echo(&runtime, &echo, BENCH_SERIAL, 1)?;
    println!(
        "serial: {:.1} us/round trip ({BENCH_SERIAL} pings)",
        serial.as_secs_f64() * 1e6 / f64::from(BENCH_SERIAL)
    );

    let mut keeper_process = KillOnDrop(spawn_node(Role::Keeper)?);
    let keeper = resolve_ref::<KeeperMessage>(&mut keeper_process)?;

    let bulk = bench_bulk(&runtime, &keeper)?;
    let mebibytes = f64::from(BENCH_BULKS) * BENCH_BULK_PAYLOAD as f64 / (1024.0 * 1024.0);
    println!(
        "bulk: {:.1} MiB/s ({BENCH_BULKS} bulks of {} KiB, window {BENCH_BULK_WINDOW})",
        mebibytes / bulk.as_secs_f64(),
        BENCH_BULK_PAYLOAD / 1024
    );

    echo.tell(Request::Stop);
    keeper.tell(KeeperMessage::Stop);
    expect_exit(&mut echo_process, "bench echo")?;
    expect_exit(&mut keeper_process, "bench keeper")?;
    Ok(())
}

fn bench_echo(
    runtime: &Runtime,
    echo: &ActorRef<Request>,
    total: u32,
    window: u32,
) -> anyhow::Result<Duration> {
    let started = Instant::now();
    let event_rx = run_client(runtime, "bench echo client termination", |event_tx| {
        BenchEcho {
            server: echo.clone(),
            total,
            window,
            event_tx,
        }
    })?;
    expect_done(&event_rx)?;
    Ok(started.elapsed())
}

fn bench_bulk(runtime: &Runtime, keeper: &ActorRef<KeeperMessage>) -> anyhow::Result<Duration> {
    let started = Instant::now();
    let event_rx = run_client(runtime, "bench bulk client termination", |event_tx| {
        BenchBulk {
            keeper: keeper.clone(),
            event_tx,
        }
    })?;
    expect_done(&event_rx)?;
    Ok(started.elapsed())
}

/// Round trip and per-sender FIFO: ordered pings, ordered replies. The reference bytes must
/// resolve as the type they were serialized for and be refused as any other. Then the node is
/// killed and told anyway, and a second node is served afterwards: only that further round trip
/// proves the dead letters left the endpoint usable, which the sends alone cannot show.
fn echo_scenario(runtime: &Runtime) -> anyhow::Result<()> {
    let mut echo_process = KillOnDrop(spawn_node(Role::Echo)?);
    let bytes = ref_bytes(&mut echo_process)?;
    let echo = remote::deserialize_ref::<Request>(&bytes).context("server reference")?;

    let mistyped = remote::deserialize_ref::<Reply>(&bytes);
    if !matches!(mistyped, Err(remote::RefError::TypeMismatch)) {
        bail!("reference of another message type resolved: {mistyped:?}");
    }

    let event_rx = run_client(runtime, "echo client termination", |event_tx| EchoClient {
        server: echo.clone(),
        event_tx,
    })?;
    expect_done(&event_rx)?;

    expect_exit(&mut echo_process, "echo")?;

    runtime.block_on(async {
        echo.tell(Request::Stop);
        sleep(DEAD_LETTER_GRACE).await;
        echo.tell(Request::Stop);
    });

    let mut echo_process = KillOnDrop(spawn_node(Role::Echo)?);
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

    expect_exit(&mut echo_process, "echo")?;
    Ok(())
}

/// Discovery: a key registered on another node resolves into a working reference, given only that
/// node's address. The lookup is issued before the node exists, so it also proves that bootstrap
/// order does not matter: the pending lookup rides the lane which dials until the node answers.
/// A wrong name and a wrong message type are distinguished, since only the first is worth
/// retrying.
fn discovery_scenario(runtime: &Runtime) -> anyhow::Result<()> {
    let addr = reserved_addr()?;

    let looking_up = runtime.spawn(async move {
        let key = Key::<Request>::new(ECHO_KEY);
        timeout(TIMEOUT, remote::lookup(&key, addr)).await
    });
    let mut echo_process = KillOnDrop(spawn_node_at(Role::Echo, Some(addr))?);

    let echo = runtime
        .block_on(looking_up)
        .context("lookup task")?
        .context("lookup before the node is up")?
        .context("lookup")?;

    let missing = runtime.block_on(async {
        let key = Key::<Request>::new("no-such-name");
        timeout(TIMEOUT, remote::lookup(&key, addr)).await
    })?;
    if !matches!(missing, Err(remote::LookupError::NotFound)) {
        bail!("unregistered name resolved to {missing:?}");
    }

    let mistyped = runtime.block_on(async {
        let key = Key::<Reply>::new(ECHO_KEY);
        timeout(TIMEOUT, remote::lookup(&key, addr)).await
    })?;
    if !matches!(mistyped, Err(remote::LookupError::TypeMismatch)) {
        bail!("key of another message type resolved to {mistyped:?}");
    }

    let event_rx = run_client(runtime, "discovered echo client termination", |event_tx| {
        EchoClient {
            server: echo,
            event_tx,
        }
    })?;
    expect_done(&event_rx)?;

    expect_exit(&mut echo_process, "discovered echo")?;
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

/// Two watchers on this node watching one remote actor must each receive a terminated signal:
/// the wire watch is per watcher, so one registration on the watched node owes two signals.
fn two_watchers_scenario(
    runtime: &Runtime,
    keeper: &ActorRef<KeeperMessage>,
) -> anyhow::Result<()> {
    let event_rx = run_client(runtime, "two watchers client termination", |event_tx| {
        TwoWatchers {
            keeper: keeper.clone(),
            event_tx,
        }
    })?;

    for _ in 0..WATCHERS {
        expect_done(&event_rx)?;
    }
    Ok(())
}

/// A large message towards one remote actor must not delay messages towards others: the busy
/// target's stream is not the one they ride. Told first and by far the largest, the busy target's
/// acknowledgement would be the first to arrive over a single lane.
fn head_of_line_scenario(
    runtime: &Runtime,
    keeper: &ActorRef<KeeperMessage>,
) -> anyhow::Result<()> {
    let event_rx = run_client(runtime, "head of line client termination", |event_tx| {
        HeadOfLine {
            keeper: keeper.clone(),
            event_tx,
        }
    })?;
    expect_done(&event_rx)
}

/// A message encoding beyond `max_frame_size` becomes a local dead letter instead of tearing down
/// the lane: an empty bulk told right behind it to the same target, riding the same stream, must
/// still be acknowledged, and the oversize one must never be.
fn oversize_scenario(runtime: &Runtime, keeper: &ActorRef<KeeperMessage>) -> anyhow::Result<()> {
    let event_rx = run_client(runtime, "oversize client termination", |event_tx| {
        Oversize {
            keeper: keeper.clone(),
            event_tx,
        }
    })?;
    expect_done(&event_rx)
}

/// Severing every connection mid-stream loses frames but never reorders or duplicates them: the
/// acknowledgements arrive strictly ascending, i.e. "in order, with gaps", and a marker told
/// after the sever arrives over the reconnected lane, proving it survived with its queue. This
/// also exercises inbound reader supersession: the reconnected connection's reader takes over
/// from the severed one.
fn sever_scenario(runtime: &Runtime, keeper: &ActorRef<KeeperMessage>) -> anyhow::Result<()> {
    let (event_tx, event_rx) = mpsc::channel();
    let system = ActorSystem::new(SeverFifo {
        keeper: keeper.clone(),
        event_tx,
    });

    match recv(&event_rx)? {
        TestEvent::Bulking => {}
        _ => bail!("unexpected event from the sever client"),
    }

    thread::sleep(SEVER_DELAY);
    if !remote::sever_connections() {
        bail!("cannot sever, endpoint not started");
    }

    let mut done = None;
    for _ in 0..MARKER_ATTEMPTS {
        system.root().tell(ClientEvent::Probe);
        match event_rx.recv_timeout(MARKER_RETRY_DELAY) {
            Ok(TestEvent::Done(result)) => {
                done = Some(result);
                break;
            }

            Ok(_) => bail!("unexpected event from the sever client"),

            Err(_) => {}
        }
    }
    match done {
        Some(Ok(())) => {}
        Some(Err(message)) => bail!(message),
        None => bail!("no marker acknowledgement after the sever"),
    }

    runtime
        .block_on(timeout(TIMEOUT, system.terminated()))
        .context("sever client termination")??;
    Ok(())
}

/// A terminated frame dropped on the watched node (fault injection) must still reach the watcher:
/// the periodic watch refresh re-asserts the watch, and the watched node answers a watch for a
/// meanwhile terminated actor with `Terminated`.
fn lost_terminated_scenario(
    runtime: &Runtime,
    keeper: &ActorRef<KeeperMessage>,
) -> anyhow::Result<()> {
    let event_rx = run_client(runtime, "lost terminated client termination", |event_tx| {
        LostTerminated {
            keeper: keeper.clone(),
            event_tx,
        }
    })?;
    expect_done(&event_rx)
}

/// An ask towards a terminated remote actor is dead-lettered on the receiving node without being
/// decoded, so no proxy exists whose drop could answer; the reply tags riding the message frame
/// let that node answer nonetheless, resolving the ask as `NoReply` rather than by timeout.
fn dead_target_ask_scenario(
    runtime: &Runtime,
    keeper: &ActorRef<KeeperMessage>,
) -> anyhow::Result<()> {
    let dead_child = terminated_child(runtime, keeper).context("terminated child")?;

    let asked =
        runtime.block_on(dead_child.ask(TIMEOUT, |reply_to| StreamerMessage::Ask { reply_to }));
    if !matches!(asked, Err(AskError::NoReply)) {
        bail!("ask towards a terminated actor resolved to {asked:?}");
    }
    Ok(())
}

/// A remote `ask` resolves with the reply; a responder dropping its `ReplyTo` resolves the ask as
/// `NoReply` via the reply-dropped notification rather than by timeout; a request beyond
/// `max_frame_size` fails the ask at the send and an oversize reply resolves it as `NoReply` via
/// the reply-dropped notification, both instead of by timeout.
fn remote_ask_scenario(runtime: &Runtime, echo: &ActorRef<Request>) -> anyhow::Result<()> {
    for seq in 0..ASKS {
        let reply = runtime
            .block_on(echo.ask(TIMEOUT, |reply_to| Request::Ask { seq, reply_to }))
            .context("ask")?;
        if reply.seq != seq {
            bail!("reply {} instead of {seq}", reply.seq);
        }
    }

    let ignored = runtime.block_on(echo.ask(TIMEOUT, |reply_to| Request::Ignore { reply_to }));
    if !matches!(ignored, Err(AskError::NoReply)) {
        bail!("ignored ask resolved to {ignored:?}");
    }

    let oversize = runtime.block_on(echo.ask(TIMEOUT, |reply_to| Request::AskOversize {
        payload: vec![0; OVERSIZE_PAYLOAD],
        reply_to,
    }));
    if !matches!(oversize, Err(AskError::ActorTerminated)) {
        bail!("oversize ask resolved to {oversize:?}");
    }

    let oversize_reply =
        runtime.block_on(echo.ask(TIMEOUT, |reply_to| Request::AskOversizeReply { reply_to }));
    if !matches!(oversize_reply, Err(AskError::NoReply)) {
        bail!("oversize reply ask resolved to {oversize_reply:?}");
    }
    Ok(())
}

/// A reply created by `reply_to` stays FIFO with the responder's other messages to the asker: the
/// server tells a marker and then replies, so the marker must arrive first.
fn reply_to_fifo_scenario(runtime: &Runtime, echo: &ActorRef<Request>) -> anyhow::Result<()> {
    let event_rx = run_client(runtime, "ask then tell client termination", |event_tx| {
        AskThenTellClient {
            server: echo.clone(),
            event_tx,
        }
    })?;
    expect_done(&event_rx)
}

/// A `ReplyTo` forwarded to a third actor still resolves its ask: the echo node re-serializes it
/// towards a responder on the client node, chaining the reply over two hops.
fn forwarded_reply_scenario(runtime: &Runtime, echo: &ActorRef<Request>) -> anyhow::Result<()> {
    let system = ActorSystem::new(Responder);
    let responder = system.root().clone();

    let reply = runtime
        .block_on(echo.ask(TIMEOUT, |reply_to| Request::Forward {
            to: responder,
            reply_to,
        }))
        .context("forwarded ask")?;
    if reply.seq != FORWARD_SEQ {
        bail!("forwarded reply {} instead of {FORWARD_SEQ}", reply.seq);
    }

    runtime
        .block_on(timeout(TIMEOUT, system.terminated()))
        .context("responder termination")??;
    Ok(())
}

/// A `ReplyTo` serialized and resolved on its own node comes home as the original destination,
/// and a second serialization of the same value is refused.
fn reply_serde_scenario(runtime: &Runtime) -> anyhow::Result<()> {
    let event_rx = run_client(runtime, "serde round trip client termination", |event_tx| {
        SerdeRoundTrip { event_tx }
    })?;
    expect_done(&event_rx)
}

/// Killing a node holding a `ReplyTo` fails the pending ask as `NoReply` via failure detection,
/// next to the synthesized terminated signal, rather than leaving it to its timeout. A probe ask
/// behind the hold proves the hold arrived before the node is killed.
fn ask_node_death_scenario(runtime: &Runtime) -> anyhow::Result<()> {
    let mut echo_process = KillOnDrop(spawn_node(Role::Echo)?);
    let echo = resolve_ref::<Request>(&mut echo_process)?;

    let (event_tx, event_rx) = mpsc::channel();
    let system = ActorSystem::new(DeathWatch {
        subject: echo.clone(),
        event_tx,
    });
    match recv(&event_rx)? {
        TestEvent::Watching => {}
        _ => bail!("unexpected event from the death watch client"),
    }

    let (probe_tx, probe_rx) = mpsc::channel();
    let held = {
        let echo = echo.clone();
        runtime.spawn(async move {
            let hold = echo.ask(TIMEOUT, |reply_to| Request::Hold { reply_to });
            let probe = async {
                let probe = echo
                    .ask(TIMEOUT, |reply_to| Request::Ask { seq: 0, reply_to })
                    .await;
                let _ = probe_tx.send(probe);
            };
            let (hold, ()) = tokio::join!(hold, probe);
            hold
        })
    };

    let probe = probe_rx.recv_timeout(TIMEOUT).context("probe reply")?;
    if !matches!(probe, Ok(Reply { seq: 0 })) {
        bail!("probe ask resolved to {probe:?}");
    }

    echo_process.0.kill().context("killing the echo process")?;
    expect_done(&event_rx)?;

    let held = runtime.block_on(held).context("held ask task")?;
    if !matches!(held, Err(AskError::NoReply)) {
        bail!("held ask resolved to {held:?}");
    }

    runtime
        .block_on(timeout(TIMEOUT, system.terminated()))
        .context("death watch client termination")??;
    Ok(())
}

/// Killing a node nothing watches leaves failure detection out of it: the pending ask is failed
/// as `NoReply` once the lane's connect attempts are exhausted, rather than by its timeout. The
/// connections are severed after the kill, since noticing the loss is the transport's business
/// and not what this scenario proves; a probe ask behind the hold proves the hold arrived before
/// the node is killed.
fn ask_give_up_scenario(runtime: &Runtime) -> anyhow::Result<()> {
    let mut echo_process = KillOnDrop(spawn_node(Role::Echo)?);
    let echo = resolve_ref::<Request>(&mut echo_process)?;

    let (probe_tx, probe_rx) = mpsc::channel();
    let held = {
        let echo = echo.clone();
        runtime.spawn(async move {
            let hold = echo.ask(GIVE_UP_TIMEOUT, |reply_to| Request::Hold { reply_to });
            let probe = async {
                let probe = echo
                    .ask(TIMEOUT, |reply_to| Request::Ask { seq: 0, reply_to })
                    .await;
                let _ = probe_tx.send(probe);
            };
            let (hold, ()) = tokio::join!(hold, probe);
            hold
        })
    };

    let probe = probe_rx.recv_timeout(TIMEOUT).context("probe reply")?;
    if !matches!(probe, Ok(Reply { seq: 0 })) {
        bail!("probe ask resolved to {probe:?}");
    }

    echo_process.0.kill().context("killing the echo process")?;
    if !remote::sever_connections() {
        bail!("cannot sever, endpoint not started");
    }

    let held = runtime.block_on(held).context("held ask task")?;
    if !matches!(held, Err(AskError::NoReply)) {
        bail!("held ask resolved to {held:?}");
    }
    Ok(())
}

/// Killing the watched actor's node must yield a synthesized terminated signal via failure
/// detection.
fn node_death_scenario(runtime: &Runtime) -> anyhow::Result<()> {
    let mut keeper_process = KillOnDrop(spawn_node(Role::Keeper)?);
    let keeper = resolve_ref::<KeeperMessage>(&mut keeper_process)?;

    let (event_tx, event_rx) = mpsc::channel();
    let system = ActorSystem::new(DeathWatch {
        subject: keeper,
        event_tx,
    });

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

/// A node restarted at its old address is a new incarnation: the client first talks to the old
/// one, which binds the lane, then watches it and kills it, so failure detection tombstones the
/// old incarnation and severs the lane; a retried lookup against the restarted node must then
/// resolve a working reference and full per-sender FIFO must hold on the fresh lane, proving that
/// a tombstone kills an incarnation, never its address.
fn restart_scenario(runtime: &Runtime) -> anyhow::Result<()> {
    let addr = reserved_addr()?;
    let mut echo_process = KillOnDrop(spawn_node_at(Role::Echo, Some(addr))?);
    let echo = resolve_ref::<Request>(&mut echo_process)?;

    let event_rx = run_client(runtime, "ping once client termination", |event_tx| {
        PingOnce {
            server: echo.clone(),
            event_tx,
        }
    })?;
    expect_done(&event_rx)?;

    let (event_tx, event_rx) = mpsc::channel();
    let system = ActorSystem::new(DeathWatch {
        subject: echo,
        event_tx,
    });
    match recv(&event_rx)? {
        TestEvent::Watching => {}
        _ => bail!("unexpected event from the death watch client"),
    }
    echo_process.0.kill().context("killing the echo process")?;
    expect_done(&event_rx)?;
    runtime
        .block_on(timeout(TIMEOUT, system.terminated()))
        .context("death watch client termination")??;

    let mut restarted_process = KillOnDrop(spawn_node_at(Role::Echo, Some(addr))?);

    let mut resolved = None;
    let mut last_error = None;
    for _ in 0..RESTART_LOOKUP_ATTEMPTS {
        let looked_up = runtime.block_on(async {
            let key = Key::<Request>::new(ECHO_KEY);
            timeout(RESTART_LOOKUP_TIMEOUT, remote::lookup(&key, addr)).await
        });
        match looked_up {
            Ok(Ok(reference)) => {
                resolved = Some(reference);
                break;
            }

            Ok(Err(error)) => {
                last_error = Some(anyhow::Error::new(error));
                runtime.block_on(sleep(RESTART_LOOKUP_DELAY));
            }

            Err(elapsed) => {
                last_error = Some(anyhow::Error::new(elapsed));
                runtime.block_on(sleep(RESTART_LOOKUP_DELAY));
            }
        }
    }
    let echo = resolved.with_context(|| match last_error {
        Some(error) => format!("no reference from the restarted node, last error: {error}"),
        None => "no reference from the restarted node".to_string(),
    })?;

    let event_rx = run_client(runtime, "echo client after restart", |event_tx| {
        EchoClient {
            server: echo,
            event_tx,
        }
    })?;
    expect_done(&event_rx)?;

    expect_exit(&mut restarted_process, "restarted echo")?;
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

/// Sends one `AskThenTell` and asserts the marker arrives before the reply.
struct AskThenTellClient {
    server: ActorRef<Request>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for AskThenTellClient {
    type Message = AskerMessage;
    type State = bool;
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        self.server.tell(Request::AskThenTell {
            marker_to: context.self_ref().clone(),
            reply_to: context.reply_to(AskerMessage::Answer),
        });
        Ok(false)
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        marker_seen: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match (incoming, marker_seen) {
            (Incoming::Message(AskerMessage::Marker), false) => Ok(Control::Continue(true)),

            (Incoming::Message(AskerMessage::Answer(_)), true) => {
                let _ = self.event_tx.send(TestEvent::Done(Ok(())));
                Ok(Control::Stop)
            }

            (Incoming::Message(AskerMessage::Answer(_)), false) => {
                let _ = self
                    .event_tx
                    .send(TestEvent::Done(Err("reply before the marker".to_string())));
                Ok(Control::Stop)
            }

            (Incoming::Message(AskerMessage::Marker), true) => {
                let _ = self
                    .event_tx
                    .send(TestEvent::Done(Err("second marker".to_string())));
                Ok(Control::Stop)
            }

            (Incoming::Terminated(_), marker_seen) => Ok(Control::Continue(marker_seen)),
        }
    }
}

/// Answers a forwarded request and stops.
struct Responder;

impl Actor for Responder {
    type Message = ForwardedRequest;
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
            Incoming::Message(ForwardedRequest { reply_to }) => {
                reply_to.reply(Reply { seq: FORWARD_SEQ });
                Ok(Control::Stop)
            }

            Incoming::Terminated(_) => Ok(Control::Continue(state)),
        }
    }
}

/// Serializes its own `ReplyTo`, asserts a second serialization is refused, resolves the bytes on
/// its own node and replies through the resolved destination into its own mailbox.
struct SerdeRoundTrip {
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for SerdeRoundTrip {
    type Message = AskerMessage;
    type State = ();
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        let fail = |message: String| {
            let _ = self.event_tx.send(TestEvent::Done(Err(message)));
            context.self_ref().tell(AskerMessage::Marker);
        };

        let reply_to = context.reply_to(AskerMessage::Answer);
        let bytes = match serde_json::to_vec(&reply_to) {
            Ok(bytes) => bytes,
            Err(error) => {
                fail(format!("serializing the reply destination: {error}"));
                return Ok(());
            }
        };

        if serde_json::to_vec(&reply_to).is_ok() {
            fail("a second serialization of the reply destination succeeded".to_string());
            return Ok(());
        }

        match serde_json::from_slice::<ReplyTo<Reply>>(&bytes) {
            Ok(reply_to) => reply_to.reply(Reply {
                seq: ROUND_TRIP_SEQ,
            }),

            Err(error) => fail(format!("resolving the reply destination bytes: {error}")),
        }
        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(AskerMessage::Answer(Reply { seq })) => {
                let result = if seq == ROUND_TRIP_SEQ {
                    Ok(())
                } else {
                    Err(format!("reply {seq} instead of {ROUND_TRIP_SEQ}"))
                };
                let _ = self.event_tx.send(TestEvent::Done(result));
                Ok(Control::Stop)
            }

            Incoming::Message(AskerMessage::Marker) => Ok(Control::Stop),

            Incoming::Terminated(_) => Ok(Control::Continue(state)),
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

            Incoming::Message(ClientEvent::Bulked(_) | ClientEvent::Armed) => {
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

/// Spawns a remote actor, has [WATCHERS] local actors watch it and stops it; it terminates only
/// once all of them have seen their signal.
struct TwoWatchers {
    keeper: ActorRef<KeeperMessage>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for TwoWatchers {
    type Message = ClientEvent;
    type State = usize;
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
        watching: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(ClientEvent::Child(child)) => {
                for _ in 0..WATCHERS {
                    let watcher = context.spawn(ChildWatcher {
                        child: child.clone(),
                        event_tx: self.event_tx.clone(),
                    });
                    context.watch(&watcher);
                }

                child.tell(StreamerMessage::Go {
                    count: 0,
                    reply_to: context.self_ref().clone(),
                });
                Ok(Control::Continue(WATCHERS))
            }

            Incoming::Terminated(_) => {
                let watching = watching - 1;
                if watching == 0 {
                    Ok(Control::Stop)
                } else {
                    Ok(Control::Continue(watching))
                }
            }

            _ => Ok(Control::Continue(watching)),
        }
    }
}

struct ChildWatcher {
    child: ActorRef<StreamerMessage>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for ChildWatcher {
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

/// Bulk messages to one of [BULK_TARGETS] remote actors, a tiny one to each of the others, all
/// told in that order: the first acknowledgement must come from one of the others.
struct HeadOfLine {
    keeper: ActorRef<KeeperMessage>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for HeadOfLine {
    type Message = ClientEvent;
    type State = HeadOfLineState;
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        for _ in 0..BULK_TARGETS {
            self.keeper.tell(KeeperMessage::Spawn {
                reply_to: context.self_ref().clone(),
            });
        }
        Ok(HeadOfLineState::AwaitingTargets(Vec::new()))
    }

    fn receive(
        &self,
        context: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match (incoming, state) {
            (
                Incoming::Message(ClientEvent::Child(child)),
                HeadOfLineState::AwaitingTargets(mut targets),
            ) => {
                targets.push(child);
                if targets.len() < BULK_TARGETS {
                    return Ok(Control::Continue(HeadOfLineState::AwaitingTargets(targets)));
                }

                let payload = vec![0; BULK_PAYLOAD];
                for seq in 0..BULKS {
                    targets[0].tell(StreamerMessage::Bulk {
                        seq,
                        payload: payload.clone(),
                        reply_to: context.self_ref().clone(),
                    });
                }
                for (index, target) in targets.iter().enumerate().skip(1) {
                    let index = u32::try_from(index).expect("the target index fits");
                    target.tell(StreamerMessage::Bulk {
                        seq: BULKS + index,
                        payload: Vec::new(),
                        reply_to: context.self_ref().clone(),
                    });
                }

                Ok(Control::Continue(HeadOfLineState::Bulking {
                    acknowledged: 0,
                    ahead: 0,
                    overtaken: false,
                }))
            }

            (
                Incoming::Message(ClientEvent::Bulked(seq)),
                HeadOfLineState::Bulking {
                    acknowledged,
                    ahead,
                    overtaken,
                },
            ) => {
                let overtaken = overtaken || seq >= BULKS;
                let ahead = ahead + u32::from(!overtaken);

                let acknowledged = acknowledged + 1;
                if acknowledged < BULK_ACKNOWLEDGEMENTS {
                    return Ok(Control::Continue(HeadOfLineState::Bulking {
                        acknowledged,
                        ahead,
                        overtaken,
                    }));
                }

                let result = if ahead < BULKS {
                    Ok(())
                } else {
                    Err(format!(
                        "all {ahead} bulk messages were acknowledged before any other target"
                    ))
                };
                let _ = self.event_tx.send(TestEvent::Done(result));
                Ok(Control::Stop)
            }

            (_, state) => Ok(Control::Continue(state)),
        }
    }
}

/// `ahead` counts the bulk target's acknowledgements arriving before any other target's, which is
/// all of them when one lane carries everything and the bulk messages were told first.
enum HeadOfLineState {
    AwaitingTargets(Vec<ActorRef<StreamerMessage>>),
    Bulking {
        acknowledged: u32,
        ahead: u32,
        overtaken: bool,
    },
}

/// Tells the streamer a bulk beyond `max_frame_size` and an empty one right behind it, expecting
/// only the empty one to be acknowledged.
struct Oversize {
    keeper: ActorRef<KeeperMessage>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for Oversize {
    type Message = ClientEvent;
    type State = ();
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        self.keeper.tell(KeeperMessage::Spawn {
            reply_to: context.self_ref().clone(),
        });
        Ok(())
    }

    fn receive(
        &self,
        context: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        match incoming {
            Incoming::Message(ClientEvent::Child(child)) => {
                child.tell(StreamerMessage::Bulk {
                    seq: 0,
                    payload: vec![0; OVERSIZE_PAYLOAD],
                    reply_to: context.self_ref().clone(),
                });
                child.tell(StreamerMessage::Bulk {
                    seq: 1,
                    payload: Vec::new(),
                    reply_to: context.self_ref().clone(),
                });
                Ok(Control::Continue(state))
            }

            Incoming::Message(ClientEvent::Bulked(seq)) => {
                let result = if seq == 1 {
                    Ok(())
                } else {
                    Err(format!("oversize bulk {seq} was acknowledged"))
                };
                let _ = self.event_tx.send(TestEvent::Done(result));
                Ok(Control::Stop)
            }

            _ => Ok(Control::Continue(state)),
        }
    }
}

/// Keeps `window` pings outstanding towards the echo server until `total` replies arrived; a
/// window of one measures serial latency, a large one pipelined throughput.
struct BenchEcho {
    server: ActorRef<Request>,
    total: u32,
    window: u32,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for BenchEcho {
    type Message = Reply;
    type State = BenchState;
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        let sent = self.window.min(self.total);
        for seq in 0..sent {
            self.server.tell(Request::Ping {
                seq,
                reply_to: context.self_ref().clone(),
            });
        }
        Ok(BenchState { sent, received: 0 })
    }

    fn receive(
        &self,
        context: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        mut state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        let Incoming::Message(Reply { .. }) = incoming else {
            return Ok(Control::Continue(state));
        };

        state.received += 1;
        if state.sent < self.total {
            self.server.tell(Request::Ping {
                seq: state.sent,
                reply_to: context.self_ref().clone(),
            });
            state.sent += 1;
        }

        if state.received == self.total {
            let _ = self.event_tx.send(TestEvent::Done(Ok(())));
            Ok(Control::Stop)
        } else {
            Ok(Control::Continue(state))
        }
    }
}

struct BenchState {
    sent: u32,
    received: u32,
}

/// Keeps [BENCH_BULK_WINDOW] bulks of [BENCH_BULK_PAYLOAD] bytes outstanding towards a streamer
/// until [BENCH_BULKS] acknowledgements arrived.
struct BenchBulk {
    keeper: ActorRef<KeeperMessage>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for BenchBulk {
    type Message = ClientEvent;
    type State = Option<BenchBulkState>;
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
        match (incoming, state) {
            (Incoming::Message(ClientEvent::Child(child)), None) => {
                let sent = BENCH_BULK_WINDOW.min(BENCH_BULKS);
                for seq in 0..sent {
                    child.tell(StreamerMessage::Bulk {
                        seq,
                        payload: vec![0; BENCH_BULK_PAYLOAD],
                        reply_to: context.self_ref().clone(),
                    });
                }
                Ok(Control::Continue(Some(BenchBulkState {
                    child,
                    counts: BenchState { sent, received: 0 },
                })))
            }

            (Incoming::Message(ClientEvent::Bulked(_)), Some(mut state)) => {
                state.counts.received += 1;
                if state.counts.sent < BENCH_BULKS {
                    state.child.tell(StreamerMessage::Bulk {
                        seq: state.counts.sent,
                        payload: vec![0; BENCH_BULK_PAYLOAD],
                        reply_to: context.self_ref().clone(),
                    });
                    state.counts.sent += 1;
                }

                if state.counts.received == BENCH_BULKS {
                    let _ = self.event_tx.send(TestEvent::Done(Ok(())));
                    Ok(Control::Stop)
                } else {
                    Ok(Control::Continue(Some(state)))
                }
            }

            (_, state) => Ok(Control::Continue(state)),
        }
    }
}

struct BenchBulkState {
    child: ActorRef<StreamerMessage>,
    counts: BenchState,
}

/// Fires a burst of bulks at the streamer and validates every acknowledgement arrives strictly
/// ascending; a `Probe` tells one further marker bulk, whose acknowledgement completes the run.
struct SeverFifo {
    keeper: ActorRef<KeeperMessage>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for SeverFifo {
    type Message = ClientEvent;
    type State = Option<SeverFifoState>;
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
        match (incoming, state) {
            (Incoming::Message(ClientEvent::Child(child)), None) => {
                let payload = vec![0; SEVER_PAYLOAD];
                for seq in 0..SEVER_BULKS {
                    child.tell(StreamerMessage::Bulk {
                        seq,
                        payload: payload.clone(),
                        reply_to: context.self_ref().clone(),
                    });
                }
                let _ = self.event_tx.send(TestEvent::Bulking);
                Ok(Control::Continue(Some(SeverFifoState {
                    child,
                    last: None,
                    next_marker: SEVER_BULKS,
                })))
            }

            (Incoming::Message(ClientEvent::Probe), Some(mut state)) => {
                state.child.tell(StreamerMessage::Bulk {
                    seq: state.next_marker,
                    payload: Vec::new(),
                    reply_to: context.self_ref().clone(),
                });
                state.next_marker += 1;
                Ok(Control::Continue(Some(state)))
            }

            (Incoming::Message(ClientEvent::Bulked(seq)), Some(mut state)) => {
                if state.last.is_some_and(|last| seq <= last) {
                    let _ = self.event_tx.send(TestEvent::Done(Err(format!(
                        "acknowledgement {seq} after {:?}",
                        state.last
                    ))));
                    return Ok(Control::Stop);
                }
                if seq >= SEVER_BULKS {
                    let _ = self.event_tx.send(TestEvent::Done(Ok(())));
                    return Ok(Control::Stop);
                }
                state.last = Some(seq);
                Ok(Control::Continue(Some(state)))
            }

            (_, state) => Ok(Control::Continue(state)),
        }
    }
}

struct SeverFifoState {
    child: ActorRef<StreamerMessage>,
    last: Option<u32>,
    next_marker: u32,
}

/// Watches the streamer, arms the keeper's endpoint to drop the next terminated frame and only
/// then stops the streamer: the signal must arrive nonetheless, through the watch refresh.
struct LostTerminated {
    keeper: ActorRef<KeeperMessage>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for LostTerminated {
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
                self.keeper.tell(KeeperMessage::DropTerminated {
                    count: 1,
                    reply_to: context.self_ref().clone(),
                });
                Ok(Control::Continue(Some(child)))
            }

            Incoming::Message(ClientEvent::Armed) => {
                if let Some(child) = &state {
                    child.tell(StreamerMessage::Go {
                        count: 0,
                        reply_to: context.self_ref().clone(),
                    });
                }
                Ok(Control::Continue(state))
            }

            Incoming::Terminated(id) => {
                let result = match &state {
                    Some(child) if child.actor_id() == id => Ok(()),
                    _ => Err(format!("terminated signal for unexpected actor {id}")),
                };
                let _ = self.event_tx.send(TestEvent::Done(result));
                Ok(Control::Stop)
            }

            _ => Ok(Control::Continue(state)),
        }
    }
}

/// Watches its subject, reports that it is watching and reports the terminated signal, whether
/// real or synthesized.
struct DeathWatch<N> {
    subject: ActorRef<N>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl<N> Actor for DeathWatch<N>
where
    N: Send + 'static,
{
    type Message = ClientEvent;
    type State = ();
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        context.watch(&self.subject);
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

        let result = if id == self.subject.actor_id() {
            Ok(())
        } else {
            Err(format!("terminated signal for unexpected actor {id}"))
        };
        let _ = self.event_tx.send(TestEvent::Done(result));
        Ok(Control::Stop)
    }
}

/// One ping, one reply: proves the lane round trips and leaves the server running.
struct PingOnce {
    server: ActorRef<Request>,
    event_tx: mpsc::Sender<TestEvent>,
}

impl Actor for PingOnce {
    type Message = Reply;
    type State = ();
    type Error = Infallible;

    fn init(&self, context: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        self.server.tell(Request::Ping {
            seq: 0,
            reply_to: context.self_ref().clone(),
        });
        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        state: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        let Incoming::Message(Reply { seq }) = incoming else {
            return Ok(Control::Continue(state));
        };

        let result = if seq == 0 {
            Ok(())
        } else {
            Err(format!("reply {seq} instead of 0"))
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

/// Start this process's remoting endpoint, on the address named by [ADDR_ENV] or otherwise an OS
/// chosen port; must run inside a Tokio runtime.
fn start_endpoint() -> anyhow::Result<SocketAddr> {
    let bind_addr = env::var(ADDR_ENV).unwrap_or_else(|_| "127.0.0.1:0".to_string());
    let transport = QuicTransport::dev(bind_addr.parse()?)?;
    let addr = transport.local_addr()?;
    remote::start(EndpointConfig::new(addr), TimeoutTransport(transport))?;
    Ok(addr)
}

/// QUIC abandons a dial towards a silent address only after its 30 s handshake timeout, which
/// would stretch a lane's give-up over minutes; every scenario waiting on failed dials needs
/// each attempt bounded instead.
struct TimeoutTransport(QuicTransport);

impl Transport for TimeoutTransport {
    type Connection = QuicConnection;

    fn data_streams(&self) -> Option<NonZeroUsize> {
        self.0.data_streams()
    }

    async fn connect(
        &self,
        addr: SocketAddr,
        max_frame_size: usize,
    ) -> Result<ConnectedControl<QuicConnection>, TransportError> {
        match timeout(CONNECT_TIMEOUT, self.0.connect(addr, max_frame_size)).await {
            Ok(connected) => connected,
            Err(_) => Err(TransportError::other("connect timed out")),
        }
    }

    async fn accept(&self, max_frame_size: usize) -> Result<QuicConnection, TransportError> {
        self.0.accept(max_frame_size).await
    }
}

/// An address the OS has just handed out and nothing holds anymore, so the node about to be
/// spawned can bind it: naming a node before it exists is what a lookup has to survive.
fn reserved_addr() -> anyhow::Result<SocketAddr> {
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    let addr = socket.local_addr()?;
    drop(socket);
    Ok(addr)
}

fn spawn_node(role: Role) -> anyhow::Result<Child> {
    spawn_node_at(role, None)
}

fn spawn_node_at(role: Role, bind_addr: Option<SocketAddr>) -> anyhow::Result<Child> {
    let mut command = Command::new(env::current_exe()?);
    command.env(ROLE_ENV, role.as_str()).stdout(Stdio::piped());
    if let Some(bind_addr) = bind_addr {
        command.env(ADDR_ENV, bind_addr.to_string());
    }

    let child = command
        .spawn()
        .with_context(|| format!("{} process", role.as_str()))?;
    Ok(child)
}

fn resolve_ref<M>(child: &mut Child) -> anyhow::Result<ActorRef<M>>
where
    M: Serialize + Send + 'static,
{
    let bytes = ref_bytes(child)?;
    remote::deserialize_ref(&bytes).context("server reference")
}

/// The bootstrap read must be bounded like every other wait in this file: a node which hangs
/// before printing its reference must fail its scenario rather than hang the whole suite.
fn ref_bytes(child: &mut Child) -> anyhow::Result<Vec<u8>> {
    let stdout = child.stdout.take().context("server process stdout")?;
    let (ref_tx, ref_rx) = mpsc::channel();

    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if let Some(hex) = line.strip_prefix(REF_PREFIX) {
                        let _ = ref_tx.send(hex_decode(hex));
                        return;
                    }
                }

                Err(error) => {
                    let _ = ref_tx.send(Err(error.into()));
                    return;
                }
            }
        }
        let _ = ref_tx.send(Err(anyhow::Error::msg(
            "no server reference on the server process stdout",
        )));
    });

    ref_rx
        .recv_timeout(TIMEOUT)
        .context("server reference within the timeout")?
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

/// Waiting without checking the exit status is what this helper makes impossible: a node which
/// died reporting an error must fail its scenario.
fn expect_exit(child: &mut Child, what: &str) -> anyhow::Result<()> {
    let status = wait_with_timeout(child, TIMEOUT)?;
    if !status.success() {
        bail!("{what} process exited with {status}");
    }
    Ok(())
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
    let (pairs, remainder) = hex.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        bail!("odd length hex encoded reference");
    }

    pairs
        .iter()
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
