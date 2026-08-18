#![cfg(feature = "persistence")]

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    convert::Infallible,
    num::{NonZeroU32, NonZeroUsize},
    sync::{Arc, Mutex},
    time::Duration,
};
use thiserror::Error;
use tokio::{
    sync::mpsc,
    time::{sleep, timeout},
};
use waltz::{
    Actor, ActorConfig, ActorContext, ActorRef, ActorSystem, AppendError, Backoff, Cbor, Codec,
    Control, Effect, EncodedEvent, EncodedSnapshot, EventSourced, EventStore, Incoming, Nothing,
    Persistence, PersistenceId, ReplyTo, RestartPolicy, SchemaVersion, SeqNo, SnapshotStore,
    StoredEvent, StoredSnapshot, SupervisionStrategy, Versioned,
};

const TIMEOUT: Duration = Duration::from_secs(5);

/// Replay reconstructs the live state: a counter increments across single and atomic multi-event
/// effects, persists a final event while stopping, and a second incarnation over the same store
/// answers with the same count, seeded by `init` and folded by `apply` alone.
#[tokio::test]
async fn replay_reconstructs_the_live_state() {
    let store = TestStore::default();
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();

    let system = ActorSystem::event_sourced(
        Counter::new("1", probe_tx.clone()),
        Persistence::new(store.clone()),
    );
    assert_eq!(recv(&mut probe_rx, "no init probe").await, Probe::Init);
    assert_eq!(
        recv(&mut probe_rx, "no recovered probe").await,
        Probe::Recovered
    );

    system.root().tell(Command::Increment(1));
    system.root().tell(Command::IncrementTwice(2, 3));
    let count = system
        .root()
        .ask(TIMEOUT, Command::Get)
        .await
        .expect("no reply to first get");
    assert_eq!(count, 6);

    system.root().tell(Command::StopAfter(4));
    assert_terminates(system, "first incarnation did not terminate").await;

    let id = persistence_id("1");
    let events = store.stream(&id);
    assert_eq!(events.len(), 4);
    assert_eq!(
        events.last().map(|stored| stored.seq_no),
        Some(SeqNo::new(3)),
        "the final event must be persisted before stopping"
    );

    let system =
        ActorSystem::event_sourced(Counter::new("1", probe_tx), Persistence::new(store.clone()));
    let count = system
        .root()
        .ask(TIMEOUT, Command::Get)
        .await
        .expect("no reply to second get");
    assert_eq!(count, 10);

    system.root().tell(Command::Stop);
    assert_terminates(system, "second incarnation did not terminate").await;
}

/// Without a snapshot every recovery seeds via `init`; with a snapshot `init` is skipped and
/// `init_from_snapshot` seeds instead; `recovered` runs on every recovery either way. A snapshot
/// also shortens replay: the second incarnation reads only the events after it.
#[tokio::test]
async fn snapshots_skip_init_and_shorten_replay() {
    let store = TestStore::default();
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();

    let system = ActorSystem::event_sourced(
        Counter::new("2", probe_tx.clone()).with_snapshot_every(3),
        Persistence::new(store.clone()).with_snapshot_store(store.clone()),
    );
    assert_eq!(recv(&mut probe_rx, "no init probe").await, Probe::Init);
    assert_eq!(
        recv(&mut probe_rx, "no recovered probe").await,
        Probe::Recovered
    );

    for _ in 0..3 {
        system.root().tell(Command::Increment(1));
    }
    let count = system
        .root()
        .ask(TIMEOUT, Command::Get)
        .await
        .expect("no reply to first get");
    assert_eq!(count, 3);

    system.root().tell(Command::Stop);
    assert_terminates(system, "first incarnation did not terminate").await;

    let system = ActorSystem::event_sourced(
        Counter::new("2", probe_tx).with_snapshot_every(3),
        Persistence::new(store.clone()).with_snapshot_store(store.clone()),
    );
    let count = system
        .root()
        .ask(TIMEOUT, Command::Get)
        .await
        .expect("no reply to second get");
    assert_eq!(count, 3);

    system.root().tell(Command::Stop);
    assert_terminates(system, "second incarnation did not terminate").await;

    let mut probes = Vec::new();
    while let Ok(probe) = probe_rx.try_recv() {
        probes.push(probe);
    }
    let second_recovery = probes
        .iter()
        .skip_while(|probe| !matches!(probe, Probe::InitFromSnapshot))
        .take(2)
        .collect::<Vec<_>>();
    assert_eq!(
        second_recovery,
        [&Probe::InitFromSnapshot, &Probe::Recovered],
        "the second recovery must seed from the snapshot and still run recovered"
    );
    assert!(
        !probes[1..].contains(&Probe::Init),
        "init must be skipped once a snapshot exists"
    );
    assert_eq!(
        store.reads(),
        [SeqNo::ZERO, SeqNo::new(3)],
        "the second recovery must replay only the events after the snapshot"
    );
}

/// A failure to save a snapshot never fails the actor: commands keep settling, and with no
/// snapshot stored the next recovery seeds via `init` and replays in full.
#[tokio::test]
async fn a_failed_snapshot_save_never_fails_the_actor() {
    let store = TestStore::default().with_failing_saves();
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();

    let system = ActorSystem::event_sourced(
        Counter::new("8", probe_tx.clone()).with_snapshot_every(1),
        Persistence::new(store.clone()).with_snapshot_store(store.clone()),
    );
    system.root().tell(Command::Increment(1));
    system.root().tell(Command::Increment(2));
    let count = system
        .root()
        .ask(TIMEOUT, Command::Get)
        .await
        .expect("no reply to first get");
    assert_eq!(count, 3);

    system.root().tell(Command::Stop);
    assert_terminates(system, "first incarnation did not terminate").await;

    let system = ActorSystem::event_sourced(
        Counter::new("8", probe_tx).with_snapshot_every(1),
        Persistence::new(store.clone()).with_snapshot_store(store.clone()),
    );
    let count = system
        .root()
        .ask(TIMEOUT, Command::Get)
        .await
        .expect("no reply to second get");
    assert_eq!(count, 3);

    system.root().tell(Command::Stop);
    assert_terminates(system, "second incarnation did not terminate").await;

    let mut probes = Vec::new();
    while let Ok(probe) = probe_rx.try_recv() {
        probes.push(probe);
    }
    assert!(
        !probes.contains(&Probe::InitFromSnapshot),
        "with every save failing, no snapshot must exist to recover from"
    );
    assert_eq!(
        store.reads(),
        [SeqNo::ZERO, SeqNo::ZERO],
        "the second recovery must replay in full"
    );
}

/// A snapshot offered without a configured snapshot store is dropped, not a failure: commands
/// keep settling and every recovery replays in full.
#[tokio::test]
async fn an_offered_snapshot_without_a_store_is_dropped() {
    let store = TestStore::default();
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();

    let system = ActorSystem::event_sourced(
        Counter::new("9", probe_tx.clone()).with_snapshot_every(1),
        Persistence::new(store.clone()),
    );
    system.root().tell(Command::Increment(1));
    let count = system
        .root()
        .ask(TIMEOUT, Command::Get)
        .await
        .expect("no reply to first get");
    assert_eq!(count, 1);

    system.root().tell(Command::Stop);
    assert_terminates(system, "first incarnation did not terminate").await;

    let system = ActorSystem::event_sourced(
        Counter::new("9", probe_tx).with_snapshot_every(1),
        Persistence::new(store.clone()),
    );
    let count = system
        .root()
        .ask(TIMEOUT, Command::Get)
        .await
        .expect("no reply to second get");
    assert_eq!(count, 1);

    system.root().tell(Command::Stop);
    assert_terminates(system, "second incarnation did not terminate").await;

    let mut probes = Vec::new();
    while let Ok(probe) = probe_rx.try_recv() {
        probes.push(probe);
    }
    assert!(
        !probes.contains(&Probe::InitFromSnapshot),
        "without a snapshot store, no snapshot must exist to recover from"
    );
    assert_eq!(
        store.reads(),
        [SeqNo::ZERO, SeqNo::ZERO],
        "the second recovery must replay in full"
    );
}

/// A `then` continuation runs once its events are settled and never on replay: across a failure
/// and restart, replaying the events emits no continuation probes.
#[tokio::test]
async fn continuations_never_run_on_replay() {
    let store = TestStore::default();
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();

    let system = ActorSystem::event_sourced_with_config(
        Counter::new("3", probe_tx),
        Persistence::new(store.clone()),
        restart_config(),
    );
    assert_eq!(recv(&mut probe_rx, "no init probe").await, Probe::Init);
    assert_eq!(
        recv(&mut probe_rx, "no recovered probe").await,
        Probe::Recovered
    );

    system.root().tell(Command::Increment(1));
    assert_eq!(
        recv(&mut probe_rx, "no handled probe").await,
        Probe::Handled
    );
    assert_eq!(
        recv(&mut probe_rx, "no settled probe").await,
        Probe::Settled(1)
    );

    system.root().tell(Command::Fail);
    assert_eq!(
        recv(&mut probe_rx, "no handled probe").await,
        Probe::Handled
    );
    assert_eq!(
        recv(&mut probe_rx, "no init probe after restart").await,
        Probe::Init
    );
    assert_eq!(
        recv(&mut probe_rx, "no recovered probe after restart").await,
        Probe::Recovered
    );

    system.root().tell(Command::Increment(2));
    assert_eq!(
        recv(&mut probe_rx, "no handled probe").await,
        Probe::Handled
    );
    assert_eq!(
        recv(&mut probe_rx, "no settled probe").await,
        Probe::Settled(3),
        "replay must restore the count without emitting settled probes"
    );

    system.root().tell(Command::Stop);
    assert_terminates(system, "system did not terminate").await;
}

/// A stale append is fenced: an event appended by another writer makes the actor's next append
/// conflict, the conflict goes through supervision, and the restarted actor replays onto the
/// winner's events instead of overwriting them; the conflicting command is consumed.
#[tokio::test]
async fn append_conflict_restarts_onto_the_winners_events() {
    let store = TestStore::default();
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();

    let system = ActorSystem::event_sourced_with_config(
        Counter::new("4", probe_tx),
        Persistence::new(store.clone()),
        restart_config(),
    );

    system.root().tell(Command::Increment(1));
    loop {
        if recv(&mut probe_rx, "no settled probe").await == Probe::Settled(1) {
            break;
        }
    }

    let id = persistence_id("4");
    store
        .append(&id, SeqNo::new(1), vec![encoded(&Increased(10))])
        .await
        .expect("the interloping append must succeed");

    system.root().tell(Command::Increment(1));
    assert_eq!(
        recv(&mut probe_rx, "no handled probe").await,
        Probe::Handled
    );
    assert_eq!(
        recv(&mut probe_rx, "no init probe after the conflict").await,
        Probe::Init
    );
    assert_eq!(
        recv(&mut probe_rx, "no recovered probe after the conflict").await,
        Probe::Recovered
    );

    let count = system
        .root()
        .ask(TIMEOUT, Command::Get)
        .await
        .expect("no reply to get");
    assert_eq!(count, 11);

    system.root().tell(Command::Stop);
    assert_terminates(system, "system did not terminate").await;
}

/// A store failure on append, unlike a conflict, has no second writer, but takes the same path:
/// supervision restarts the actor, replay reconciles with the store, and the command whose
/// append failed is consumed, its event never appended.
#[tokio::test]
async fn an_append_store_failure_restarts_and_consumes_the_command() {
    let store = TestStore::default().with_append_failures(1);
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();

    let system = ActorSystem::event_sourced_with_config(
        Counter::new("10", probe_tx),
        Persistence::new(store.clone()),
        restart_config(),
    );
    assert_eq!(recv(&mut probe_rx, "no init probe").await, Probe::Init);
    assert_eq!(
        recv(&mut probe_rx, "no recovered probe").await,
        Probe::Recovered
    );

    system.root().tell(Command::Increment(1));
    assert_eq!(
        recv(&mut probe_rx, "no handled probe").await,
        Probe::Handled
    );
    assert_eq!(
        recv(&mut probe_rx, "no init probe after the failed append").await,
        Probe::Init
    );
    assert_eq!(
        recv(&mut probe_rx, "no recovered probe after the failed append").await,
        Probe::Recovered
    );

    let count = system
        .root()
        .ask(TIMEOUT, Command::Get)
        .await
        .expect("no reply to first get");
    assert_eq!(count, 0, "the failed command's event must not be appended");

    system.root().tell(Command::Increment(2));
    let count = system
        .root()
        .ask(TIMEOUT, Command::Get)
        .await
        .expect("no reply to second get");
    assert_eq!(count, 2);

    system.root().tell(Command::Stop);
    assert_terminates(system, "system did not terminate").await;
}

/// A failure of `recovered` is an ordinary startup failure: under `Restart` the next attempt
/// runs recovery again, and once `recovered` succeeds the actor handles commands normally.
#[tokio::test]
async fn a_recovered_failure_restarts_into_a_working_actor() {
    let store = TestStore::default();
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();

    let system = ActorSystem::event_sourced_with_config(
        Counter::new("11", probe_tx).with_recovered_failures(1),
        Persistence::new(store.clone()),
        restart_config(),
    );
    assert_eq!(recv(&mut probe_rx, "no init probe").await, Probe::Init);
    assert_eq!(
        recv(&mut probe_rx, "no init probe after restart").await,
        Probe::Init
    );
    assert_eq!(
        recv(&mut probe_rx, "no recovered probe").await,
        Probe::Recovered
    );

    system.root().tell(Command::Increment(1));
    let count = system
        .root()
        .ask(TIMEOUT, Command::Get)
        .await
        .expect("no reply to get");
    assert_eq!(count, 1);

    system.root().tell(Command::Stop);
    assert_terminates(system, "system did not terminate").await;
}

/// Writes are strict: a command is fully settled, its continuations included, before the next one
/// is handled, even while the append itself is slow.
#[tokio::test]
async fn a_command_settles_before_the_next_is_handled() {
    let store = TestStore::default().with_append_delay(Duration::from_millis(20));
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();

    let system =
        ActorSystem::event_sourced(Counter::new("5", probe_tx), Persistence::new(store.clone()));
    assert_eq!(recv(&mut probe_rx, "no init probe").await, Probe::Init);
    assert_eq!(
        recv(&mut probe_rx, "no recovered probe").await,
        Probe::Recovered
    );

    system.root().tell(Command::Increment(1));
    system.root().tell(Command::Increment(2));

    let probes = [
        recv(&mut probe_rx, "no first probe").await,
        recv(&mut probe_rx, "no second probe").await,
        recv(&mut probe_rx, "no third probe").await,
        recv(&mut probe_rx, "no fourth probe").await,
    ];
    assert_eq!(
        probes,
        [
            Probe::Handled,
            Probe::Settled(1),
            Probe::Handled,
            Probe::Settled(3)
        ]
    );

    system.root().tell(Command::Stop);
    assert_terminates(system, "system did not terminate").await;
}

/// A watched actor's termination is delivered to `handle` as an ordinary incoming signal.
#[tokio::test]
async fn terminated_signals_reach_handle() {
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();

    let system = ActorSystem::event_sourced(
        Watcher {
            probe_tx,
            bye_before_stopping: false,
        },
        Persistence::new(TestStore::default()),
    );

    system.root().tell(WatcherCommand::StopChild);
    assert_eq!(
        recv(&mut probe_rx, "no terminated probe").await,
        Probe::Terminated
    );

    assert_terminates(system, "system did not terminate").await;
}

/// After `unwatch` no terminated signal is received, even if it is already enqueued: the child
/// says bye through the shared FIFO right before stopping, the watcher unwatches on the bye, and
/// the signal queued behind it is dropped before `handle` ever sees it.
#[tokio::test]
async fn unwatch_drops_an_enqueued_terminated_signal() {
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();

    let system = ActorSystem::event_sourced(
        Watcher {
            probe_tx,
            bye_before_stopping: true,
        },
        Persistence::new(TestStore::default()),
    );

    system.root().tell(WatcherCommand::StopChild);
    assert_eq!(recv(&mut probe_rx, "no bye probe").await, Probe::Bye);

    sleep(Duration::from_millis(100)).await;
    system.root().tell(WatcherCommand::Stop);
    assert_terminates(system, "system did not terminate").await;

    let mut probes = Vec::new();
    while let Ok(probe) = probe_rx.try_recv() {
        probes.push(probe);
    }
    assert!(
        !probes.contains(&Probe::Terminated),
        "no terminated signal must be received after unwatch"
    );
}

/// An undecodable snapshot is not a failure: it is discarded, recovery falls back to full
/// replay seeded by `init`, and the state still comes out right.
#[tokio::test]
async fn undecodable_snapshot_falls_back_to_full_replay() {
    let store = TestStore::default();
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();

    let id = persistence_id("7");
    for (seq_no, increment) in [(0, 1), (1, 2)] {
        store.seed(
            &id,
            StoredEvent {
                seq_no: SeqNo::new(seq_no),
                event: encoded(&Increased(increment)),
            },
        );
    }
    store.seed_snapshot(
        &id,
        StoredSnapshot {
            next_seq_no: SeqNo::new(2),
            snapshot: EncodedSnapshot {
                manifest: Count::MANIFEST.to_string(),
                schema_version: SchemaVersion::new(99),
                payload: Vec::new(),
            },
        },
    );

    let system = ActorSystem::event_sourced(
        Counter::new("7", probe_tx),
        Persistence::new(store.clone()).with_snapshot_store(store.clone()),
    );
    assert_eq!(recv(&mut probe_rx, "no init probe").await, Probe::Init);
    assert_eq!(
        recv(&mut probe_rx, "no recovered probe").await,
        Probe::Recovered
    );

    let count = system
        .root()
        .ask(TIMEOUT, Command::Get)
        .await
        .expect("no reply to get");
    assert_eq!(count, 3);

    system.root().tell(Command::Stop);
    assert_terminates(system, "system did not terminate").await;
}

/// A history the current code cannot decode is a recovery failure: under the default `Stop`
/// strategy the actor stops without ever running `recovered`, and its watchers learn about it.
#[tokio::test]
async fn undecodable_history_stops_the_actor() {
    let store = TestStore::default();
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();

    let id = persistence_id("6");
    store.seed(
        &id,
        StoredEvent {
            seq_no: SeqNo::ZERO,
            event: EncodedEvent {
                manifest: Increased::MANIFEST.to_string(),
                schema_version: SchemaVersion::new(99),
                payload: Vec::new(),
            },
        },
    );

    let system =
        ActorSystem::event_sourced(Counter::new("6", probe_tx), Persistence::new(store.clone()));
    assert_terminates(system, "the actor must stop on an undecodable history").await;

    let mut probes = Vec::new();
    while let Ok(probe) = probe_rx.try_recv() {
        probes.push(probe);
    }
    assert_eq!(
        probes,
        [Probe::Init],
        "recovery must fail after init and before recovered"
    );
}

fn persistence_id(entity_id: &str) -> PersistenceId {
    PersistenceId::new("counter", entity_id).expect("the segments are valid")
}

fn encoded(event: &Increased) -> EncodedEvent {
    EncodedEvent {
        manifest: Increased::MANIFEST.to_string(),
        schema_version: Increased::VERSION,
        payload: Cbor.encode(event).expect("the event is encodable"),
    }
}

async fn recv<T>(probe_rx: &mut mpsc::UnboundedReceiver<T>, not_received: &str) -> T {
    timeout(TIMEOUT, probe_rx.recv())
        .await
        .expect(not_received)
        .expect("probe channel closed")
}

async fn assert_terminates<M>(system: ActorSystem<M>, not_terminated: &str)
where
    M: Send + 'static,
{
    timeout(TIMEOUT, system.terminated())
        .await
        .expect(not_terminated)
        .expect("watching the root actor failed");
}

fn restart_config() -> ActorConfig {
    ActorConfig::default().with_supervision_strategy(SupervisionStrategy::Restart(
        RestartPolicy::new(NonZeroU32::new(5).expect("5 is not zero")).with_backoff(
            Backoff::new(Duration::from_millis(1), Duration::from_millis(10))
                .expect("the bounds are ordered"),
        ),
    ))
}

struct Counter {
    entity_id: &'static str,
    probe_tx: mpsc::UnboundedSender<Probe>,
    snapshot_every: Option<u64>,
    recovered_failures: Mutex<u32>,
}

impl Counter {
    fn new(entity_id: &'static str, probe_tx: mpsc::UnboundedSender<Probe>) -> Self {
        Self {
            entity_id,
            probe_tx,
            snapshot_every: None,
            recovered_failures: Mutex::new(0),
        }
    }

    fn with_snapshot_every(mut self, snapshot_every: u64) -> Self {
        self.snapshot_every = Some(snapshot_every);
        self
    }

    fn with_recovered_failures(mut self, failures: u32) -> Self {
        self.recovered_failures = Mutex::new(failures);
        self
    }
}

impl EventSourced for Counter {
    type Command = Command;
    type Event = Increased;
    type State = u64;
    type Snapshot = Count;
    type Error = Boom;

    fn persistence_id(&self) -> PersistenceId {
        persistence_id(self.entity_id)
    }

    fn init(&self) -> Result<Self::State, Self::Error> {
        let _ = self.probe_tx.send(Probe::Init);
        Ok(0)
    }

    fn init_from_snapshot(&self, Count(count): Self::Snapshot) -> Result<Self::State, Self::Error> {
        let _ = self.probe_tx.send(Probe::InitFromSnapshot);
        Ok(count)
    }

    fn recovered(
        &self,
        _: &ActorContext<Self::Command>,
        state: Self::State,
    ) -> Result<Self::State, Self::Error> {
        {
            let mut failures = self
                .recovered_failures
                .lock()
                .expect("recovered failures lock poisoned");
            if *failures > 0 {
                *failures -= 1;
                return Err(Boom);
            }
        }

        let _ = self.probe_tx.send(Probe::Recovered);
        Ok(state)
    }

    fn handle(
        &self,
        _: &ActorContext<Self::Command>,
        incoming: Incoming<Self::Command>,
        _: &Self::State,
    ) -> Result<Effect<Self>, Self::Error> {
        let Incoming::Message(command) = incoming else {
            return Ok(Effect::none());
        };
        let _ = self.probe_tx.send(Probe::Handled);

        match command {
            Command::Increment(n) => {
                let probe_tx = self.probe_tx.clone();
                Ok(Effect::persist(Increased(n)).then(move |count| {
                    let _ = probe_tx.send(Probe::Settled(*count));
                }))
            }

            Command::IncrementTwice(n, m) => Ok(Effect::persist_all([Increased(n), Increased(m)])),

            Command::Get(reply_to) => Ok(Effect::none().then(move |count| reply_to.reply(*count))),

            Command::Fail => Err(Boom),

            Command::Stop => Ok(Effect::stop()),

            Command::StopAfter(n) => Ok(Effect::persist(Increased(n)).and_stop()),
        }
    }

    fn apply(&self, state: Self::State, Increased(n): Self::Event) -> Self::State {
        state + n
    }

    fn snapshot(&self, state: &Self::State) -> Result<Option<Self::Snapshot>, Self::Error> {
        match self.snapshot_every {
            Some(every) if state % every == 0 => Ok(Some(Count(*state))),
            _ => Ok(None),
        }
    }
}

#[derive(Debug)]
enum Command {
    Increment(u64),
    IncrementTwice(u64, u64),
    Get(ReplyTo<u64>),
    Fail,
    Stop,
    StopAfter(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Increased(u64);

impl Versioned for Increased {
    const MANIFEST: &'static str = "increased";
    const VERSION: SchemaVersion = SchemaVersion::new(1);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Count(u64);

impl Versioned for Count {
    const MANIFEST: &'static str = "count";
    const VERSION: SchemaVersion = SchemaVersion::new(1);
}

#[derive(Debug, Error)]
#[error("boom")]
struct Boom;

#[derive(Debug, PartialEq, Eq)]
enum Probe {
    Init,
    InitFromSnapshot,
    Recovered,
    Handled,
    Settled(u64),
    Bye,
    Terminated,
}

struct Watcher {
    probe_tx: mpsc::UnboundedSender<Probe>,
    bye_before_stopping: bool,
}

impl EventSourced for Watcher {
    type Command = WatcherCommand;
    type Event = Nothing;
    type State = Option<ActorRef<()>>;
    type Snapshot = Nothing;
    type Error = Infallible;

    fn persistence_id(&self) -> PersistenceId {
        persistence_id("watcher")
    }

    fn init(&self) -> Result<Self::State, Self::Error> {
        Ok(None)
    }

    fn init_from_snapshot(&self, snapshot: Self::Snapshot) -> Result<Self::State, Self::Error> {
        match snapshot {}
    }

    fn recovered(
        &self,
        context: &ActorContext<Self::Command>,
        _: Self::State,
    ) -> Result<Self::State, Self::Error> {
        let child = context.spawn(Child {
            parent: self.bye_before_stopping.then(|| context.self_ref().clone()),
        });
        context.watch(&child);

        Ok(Some(child))
    }

    fn handle(
        &self,
        context: &ActorContext<Self::Command>,
        incoming: Incoming<Self::Command>,
        state: &Self::State,
    ) -> Result<Effect<Self>, Self::Error> {
        match incoming {
            Incoming::Message(WatcherCommand::StopChild) => {
                if let Some(child) = state {
                    child.tell(());
                }
                Ok(Effect::none())
            }

            Incoming::Message(WatcherCommand::Bye) => {
                let _ = self.probe_tx.send(Probe::Bye);
                if let Some(child) = state {
                    context.unwatch(child);
                }
                Ok(Effect::none())
            }

            Incoming::Message(WatcherCommand::Stop) => Ok(Effect::stop()),

            Incoming::Terminated(_) => {
                let _ = self.probe_tx.send(Probe::Terminated);
                Ok(Effect::stop())
            }
        }
    }

    fn apply(&self, _: Self::State, event: Self::Event) -> Self::State {
        match event {}
    }
}

#[derive(Debug)]
enum WatcherCommand {
    StopChild,
    Bye,
    Stop,
}

struct Child {
    parent: Option<ActorRef<WatcherCommand>>,
}

impl Actor for Child {
    type Message = ();
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
            Incoming::Message(()) => {
                if let Some(parent) = &self.parent {
                    parent.tell(WatcherCommand::Bye);
                }
                Ok(Control::Stop)
            }

            Incoming::Terminated(_) => Ok(Control::Continue(state)),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct TestStore {
    streams: Arc<Mutex<HashMap<PersistenceId, Vec<StoredEvent>>>>,
    snapshots: Arc<Mutex<HashMap<PersistenceId, StoredSnapshot>>>,
    reads: Arc<Mutex<Vec<SeqNo>>>,
    append_delay: Option<Duration>,
    append_failures: Arc<Mutex<u32>>,
    fail_saves: bool,
}

impl TestStore {
    fn with_append_delay(mut self, append_delay: Duration) -> Self {
        self.append_delay = Some(append_delay);
        self
    }

    fn with_append_failures(mut self, failures: u32) -> Self {
        self.append_failures = Arc::new(Mutex::new(failures));
        self
    }

    fn with_failing_saves(mut self) -> Self {
        self.fail_saves = true;
        self
    }

    fn seed(&self, id: &PersistenceId, stored: StoredEvent) {
        self.streams
            .lock()
            .expect("streams lock poisoned")
            .entry(id.clone())
            .or_default()
            .push(stored);
    }

    fn seed_snapshot(&self, id: &PersistenceId, stored: StoredSnapshot) {
        self.snapshots
            .lock()
            .expect("snapshots lock poisoned")
            .insert(id.clone(), stored);
    }

    fn stream(&self, id: &PersistenceId) -> Vec<StoredEvent> {
        self.streams
            .lock()
            .expect("streams lock poisoned")
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    fn reads(&self) -> Vec<SeqNo> {
        self.reads.lock().expect("reads lock poisoned").clone()
    }
}

impl EventStore for TestStore {
    type Error = TestStoreError;

    async fn append(
        &self,
        id: &PersistenceId,
        next_seq_no: SeqNo,
        events: Vec<EncodedEvent>,
    ) -> Result<(), AppendError<Self::Error>> {
        if let Some(append_delay) = self.append_delay {
            sleep(append_delay).await;
        }

        {
            let mut failures = self
                .append_failures
                .lock()
                .expect("append failures lock poisoned");
            if *failures > 0 {
                *failures -= 1;
                return Err(AppendError::Store(TestStoreError));
            }
        }

        let mut streams = self.streams.lock().expect("streams lock poisoned");
        let stream = streams.entry(id.clone()).or_default();
        if SeqNo::new(stream.len() as u64) != next_seq_no {
            return Err(AppendError::Conflict);
        }

        for (n, event) in events.into_iter().enumerate() {
            stream.push(StoredEvent {
                seq_no: next_seq_no.advanced_by(n),
                event,
            });
        }

        Ok(())
    }

    async fn read(
        &self,
        id: &PersistenceId,
        from_seq_no: SeqNo,
        limit: NonZeroUsize,
    ) -> Result<Vec<StoredEvent>, Self::Error> {
        self.reads
            .lock()
            .expect("reads lock poisoned")
            .push(from_seq_no);

        let streams = self.streams.lock().expect("streams lock poisoned");
        let events = streams
            .get(id)
            .map(|stream| {
                stream
                    .iter()
                    .filter(|stored| stored.seq_no >= from_seq_no)
                    .take(limit.get())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        Ok(events)
    }
}

impl SnapshotStore for TestStore {
    type Error = TestStoreError;

    async fn save(
        &self,
        id: &PersistenceId,
        next_seq_no: SeqNo,
        snapshot: EncodedSnapshot,
    ) -> Result<(), Self::Error> {
        if self.fail_saves {
            return Err(TestStoreError);
        }

        self.snapshots
            .lock()
            .expect("snapshots lock poisoned")
            .insert(
                id.clone(),
                StoredSnapshot {
                    next_seq_no,
                    snapshot,
                },
            );

        Ok(())
    }

    async fn load(&self, id: &PersistenceId) -> Result<Option<StoredSnapshot>, Self::Error> {
        let snapshot = self
            .snapshots
            .lock()
            .expect("snapshots lock poisoned")
            .get(id)
            .cloned();

        Ok(snapshot)
    }
}

#[derive(Debug, Error)]
#[error("test store failure")]
struct TestStoreError;
