use crate::{
    ActorId, Backoff,
    quota::{CountedSendError, CountedSender, Quota},
    remote::{
        codec::{Codec, Postcard},
        discovery::PendingLookups,
        failure::{DeadlineFailureDetector, DeliveryGate, FailureDetector, Liveness},
        frame::Frame,
        node::{Incarnation, NodeId},
        peer::{DialRequest, accept_loop, dial_loop},
        registry::Registry,
        reply::PendingReplies,
        transport::Transport,
        watch::{WatcherTable, WireWatchTable},
    },
    sync::{lock, read, write},
};
use derive_more::Debug;
use flume::Sender;
use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    hash::{DefaultHasher, Hash, Hasher},
    net::SocketAddr,
    num::NonZeroUsize,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use thiserror::Error;
use tokio::{
    sync::watch,
    task::{self, JoinHandle},
    time::{MissedTickBehavior, interval},
};
use tracing::{debug, error, warn};

static ENDPOINT: OnceLock<EndpointInner> = OnceLock::new();

/// Creates the [FailureDetector] for a peer node.
pub type FailureDetectorFactory = Arc<dyn Fn() -> Box<dyn FailureDetector> + Send + Sync>;

/// Configuration for [start].
#[derive(Debug)]
pub struct EndpointConfig {
    /// The address other nodes reach this node at; together with a per-process incarnation it is
    /// the node's identity carried inside serialized references.
    pub advertised_addr: SocketAddr,

    /// The capacity of the outbound queue per peer node; messages told to a full queue are
    /// dropped as dead letters, while system frames like terminated signals bypass the capacity
    /// and are never dropped. The capacity covers the whole peer, however many streams carry it.
    pub outbound_capacity: NonZeroUsize,

    /// The upper bound on the data streams opened per peer connection, which frames are spread
    /// over by the actor they are delivered to, so a large message only delays frames towards
    /// targets hashing onto the same stream. A transport without streams lowers this to a single
    /// lane carrying everything, and `1` does the same for one with.
    ///
    /// All of them are opened at connection setup, so a peer admitting fewer concurrent streams
    /// fails the connection right there instead of stalling a stream later; QUIC peers admit 100
    /// by default, which is the ceiling to stay under.
    pub max_streams_per_peer: NonZeroUsize,

    /// The maximum size of an encoded frame in bytes: an outbound frame beyond it becomes a
    /// local dead letter before reaching the transport, an inbound one is refused.
    pub max_frame_size: NonZeroUsize,

    /// The codec encoding and decoding message payloads.
    #[debug(skip)]
    pub codec: Arc<dyn Codec>,

    /// The interval for `Ping` frames towards the nodes a watch names in either direction, i.e.
    /// those with watched actors and those watching actors here; also the tick of the failure
    /// detection.
    pub heartbeat_interval: Duration,

    /// The interval at which this node's remote watches are re-asserted with idempotent `Watch`
    /// frames, healing a `Terminated` frame lost with the watched side's connection. Must stay
    /// below the failure detection deadline, else such a loss on a pair with no other watches is
    /// healed by a false node death instead of by the watched node's answer.
    pub watch_refresh_interval: Duration,

    /// Creates the [FailureDetector] for a peer node, deciding when it is declared dead; the
    /// default is a [DeadlineFailureDetector] with a five second deadline.
    #[debug(skip)]
    pub failure_detector: FailureDetectorFactory,

    /// The bounds pacing reconnection of a lost connection: the first attempt waits the minimum,
    /// each further one doubles it up to the maximum.
    pub reconnect_backoff: Backoff,

    /// After this many failed connection attempts a node no watch names is given up and its
    /// queued messages become dead letters; a node a watch names in either direction is retried
    /// until the failure detector declares it dead. Giving up is not final: a later message
    /// dials again. An address which answered without speaking this protocol is refused instead,
    /// until a handshake from it proves a waltz node is there.
    pub max_connect_attempts: u32,
}

impl EndpointConfig {
    /// A configuration with the given advertised address and defaults: an outbound capacity of
    /// 8192 messages, at most 16 data streams per peer, a maximum frame size of 1 MiB, the
    /// [Postcard] codec, a heartbeat interval of one second, a watch refresh interval of two
    /// seconds, a five second failure detection deadline and reconnect backoff from 250 ms to 3 s
    /// with at most 8 attempts for unwatched nodes.
    pub fn new(advertised_addr: SocketAddr) -> Self {
        Self {
            advertised_addr,
            outbound_capacity: const { NonZeroUsize::new(8_192).expect("8192 is not zero") },
            max_streams_per_peer: const { NonZeroUsize::new(16).expect("16 is not zero") },
            max_frame_size: const { NonZeroUsize::new(1_024 * 1_024).expect("1 MiB is not zero") },
            codec: Arc::new(Postcard),
            heartbeat_interval: Duration::from_secs(1),
            watch_refresh_interval: Duration::from_secs(2),
            failure_detector: Arc::new(|| {
                Box::new(DeadlineFailureDetector::new(Duration::from_secs(5)))
            }),
            reconnect_backoff: Backoff::new(Duration::from_millis(250), Duration::from_secs(3))
                .expect("the bounds are ordered"),
            max_connect_attempts: 8,
        }
    }
}

/// The remoting endpoint cannot be started.
#[derive(Debug, Error)]
pub enum StartError {
    /// The endpoint has already been started; there is one per process.
    #[error("remoting endpoint already started")]
    AlreadyStarted,
}

/// Start the process wide remoting endpoint: accept connections from the given transport and
/// dial peers on demand. Can only be called once per process; references can only be serialized
/// and deserialized after this.
///
/// # Panics
/// Panics if called outside of a Tokio runtime.
pub fn start<T>(config: EndpointConfig, transport: T) -> Result<(), StartError>
where
    T: Transport,
{
    let (dial_request_tx, dial_request_rx) = flume::unbounded();
    let inner = EndpointInner {
        node: NodeId::new(config.advertised_addr),
        data_streams: transport.data_streams().map_or(0, |streams| {
            config.max_streams_per_peer.get().min(streams.get())
        }),
        config,
        registry: Arc::new(Registry::new()),
        lanes: RwLock::new(HashMap::new()),
        inbound_readers: tokio::sync::Mutex::new(HashMap::new()),
        next_generation: AtomicU64::new(0),
        tombstones: RwLock::new(HashSet::new()),
        addr_states: Mutex::new(HashMap::new()),
        liveness: Liveness::new(),
        watchers: WatcherTable::new(),
        wire_watches: WireWatchTable::new(),
        pending_lookups: PendingLookups::new(),
        pending_replies: PendingReplies::new(),
        dial_request_tx,
        sever_tx: watch::channel(0).0,
        #[cfg(feature = "remote-dev")]
        dropped_terminated: AtomicU64::new(0),
    };
    ENDPOINT
        .set(inner)
        .map_err(|_| StartError::AlreadyStarted)?;
    let endpoint = ENDPOINT.get().expect("endpoint was just set");

    let transport = Arc::new(transport);
    task::spawn(accept_loop(transport.clone(), endpoint));
    task::spawn(dial_loop(transport, dial_request_rx, endpoint));
    task::spawn(liveness_loop(endpoint));
    task::spawn(watch_refresh_loop(endpoint));
    Ok(())
}

/// A task and the entry it was started for must agree, so a stale task cannot close a newer one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Generation(u64);

#[derive(Debug, Error)]
pub(crate) enum LaneError {
    #[error("node {0} unreachable")]
    NodeUnreachable(NodeId),

    #[error("node at {0} unreachable")]
    AddrUnreachable(SocketAddr),

    #[error("outbound queue towards node {0} full")]
    OutboundQueueFull(NodeId),
}

/// How a heartbeated peer relates to this node: watched by an actor here, so its silence means
/// node death, or merely watching actors here, so its silence only ends the tracking.
pub(crate) enum PeerRole {
    WatchedFromHere,
    WatchingHere,
}

pub(crate) struct EndpointInner {
    node: NodeId,
    data_streams: usize,
    config: EndpointConfig,
    registry: Arc<Registry>,
    lanes: RwLock<HashMap<SocketAddr, Lane>>,
    inbound_readers: tokio::sync::Mutex<HashMap<SocketAddr, InboundReader>>,
    next_generation: AtomicU64,
    tombstones: RwLock<HashSet<Incarnation>>,
    addr_states: Mutex<HashMap<SocketAddr, AddrState>>,
    liveness: Liveness,
    watchers: WatcherTable,
    wire_watches: WireWatchTable,
    pending_lookups: PendingLookups,
    pending_replies: PendingReplies,
    dial_request_tx: Sender<DialRequest>,
    sever_tx: watch::Sender<u64>,
    #[cfg(feature = "remote-dev")]
    dropped_terminated: AtomicU64,
}

impl EndpointInner {
    pub(crate) fn node(&self) -> NodeId {
        self.node
    }

    pub(crate) fn config(&self) -> &EndpointConfig {
        &self.config
    }

    pub(crate) fn codec(&self) -> &dyn Codec {
        self.config.codec.as_ref()
    }

    pub(crate) fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    pub(crate) fn watchers(&self) -> &WatcherTable {
        &self.watchers
    }

    pub(crate) fn wire_watches(&self) -> &WireWatchTable {
        &self.wire_watches
    }

    pub(crate) fn pending_lookups(&self) -> &PendingLookups {
        &self.pending_lookups
    }

    pub(crate) fn pending_replies(&self) -> &PendingReplies {
        &self.pending_replies
    }

    pub(crate) fn reconnect_backoff(&self, attempts: u32) -> Duration {
        self.config
            .reconnect_backoff
            .duration(attempts.saturating_sub(1))
    }

    /// Message frames are subject to the outbound capacity, system frames bypass it; all of them
    /// ride the one lane towards the peer, which is dialed on first use, on the stream their
    /// recipient picks, which is what makes them FIFO per recipient.
    ///
    /// A lane belongs to an address but serves one incarnation: once [EndpointInner::bind_lane] has
    /// named the handshaked peer, a frame for any other incarnation there is refused rather than
    /// written onto its successor's connection.
    ///
    /// The steady state sends under the read lock, so senders towards different peers do not
    /// serialize and no sender is cloned per frame; the opening path has to look again under the
    /// write lock, since another sender may have opened the lane in between.
    pub(crate) fn send(&self, peer: NodeId, frame: Frame<'static>) -> Result<(), LaneError> {
        if self.tombstoned(peer.incarnation()) {
            return Err(LaneError::NodeUnreachable(peer));
        }

        #[cfg(feature = "remote-dev")]
        if matches!(frame, Frame::Terminated { .. }) && self.drop_terminated() {
            debug!(%peer, "dropping terminated frame, fault injection");
            return Ok(());
        }

        {
            let lanes = read(&self.lanes);
            if let Some(outbound_tx) = Self::matching_lane(&lanes, peer, &frame)? {
                return Self::try_send(outbound_tx, frame, peer);
            }
        }

        let mut lanes = write(&self.lanes);
        if let Some(outbound_tx) = Self::matching_lane(&lanes, peer, &frame)? {
            return Self::try_send(outbound_tx, frame, peer);
        }
        if self.is_refused(peer.addr()) {
            return Err(LaneError::NodeUnreachable(peer));
        }

        let outbound_tx = self.open_lane(&mut lanes, peer.addr(), &frame);
        Self::try_send(outbound_tx, frame, peer)
    }

    /// Discovery only, and only for system frames: a lookup knows an address but no incarnation,
    /// and [NodeId::new] mints a fresh one rather than naming whoever is there, so this takes the
    /// lane at the address as it finds it and lets the answer name the incarnation which replied.
    pub(crate) fn send_to_addr(
        &self,
        addr: SocketAddr,
        frame: Frame<'static>,
    ) -> Result<(), LaneError> {
        debug_assert!(!frame.is_message(), "only system frames go to an address");

        {
            let lanes = read(&self.lanes);
            if let Some(lane) = lanes.get(&addr) {
                return lane
                    .outbound_tx(&frame)
                    .try_send_uncounted(frame)
                    .map_err(|_| LaneError::AddrUnreachable(addr));
            }
        }

        let mut lanes = write(&self.lanes);
        if let Some(lane) = lanes.get(&addr) {
            return lane
                .outbound_tx(&frame)
                .try_send_uncounted(frame)
                .map_err(|_| LaneError::AddrUnreachable(addr));
        }
        if self.is_refused(addr) {
            return Err(LaneError::AddrUnreachable(addr));
        }

        let outbound_tx = self.open_lane(&mut lanes, addr, &frame);
        outbound_tx
            .try_send_uncounted(frame)
            .map_err(|_| LaneError::AddrUnreachable(addr))
    }

    /// An incarnation change at a known address proves the previous incarnation is gone and
    /// triggers its death. The returned gate must be held by every inbound delivery from the
    /// peer.
    pub(crate) fn note_peer(&self, peer: NodeId) -> DeliveryGate {
        let gate = self.track_liveness(peer);

        let previous = lock(&self.addr_states).insert(peer.addr(), AddrState::Known(peer));
        if let Some(AddrState::Known(previous)) = previous
            && previous != peer
            && !self.tombstoned(previous.incarnation())
        {
            warn!(%peer, %previous, "node death, address reused by a new incarnation");
            self.node_death(previous);
        }

        gate
    }

    pub(crate) fn track_liveness(&self, peer: NodeId) -> DeliveryGate {
        self.liveness
            .track(peer.incarnation(), &self.config.failure_detector)
    }

    pub(crate) fn record_heartbeat(&self, incarnation: Incarnation) {
        self.liveness.record_heartbeat(incarnation);
    }

    pub(crate) fn tombstoned(&self, incarnation: Incarnation) -> bool {
        read(&self.tombstones).contains(&incarnation)
    }

    pub(crate) fn refuse(&self, addr: SocketAddr) {
        lock(&self.addr_states).insert(addr, AddrState::Refused);
    }

    /// Tombstone, close the lane, wait out in flight deliveries, only then fail the pending asks
    /// and flush the synthesized signals: this order makes the contract true by construction,
    /// i.e. after the signal no message from that node is ever delivered again, and an ask failed
    /// as `NoReply` is never followed by its reply.
    ///
    /// Only for a node an actor here watches: the tombstone is permanent, which is the price of
    /// backing a synthesized signal. A silent peer nothing watches goes to
    /// [EndpointInner::untrack_peer] instead.
    pub(crate) fn node_death(&self, peer: NodeId) {
        if !write(&self.tombstones).insert(peer.incarnation()) {
            return;
        }

        {
            // An unbound lane is still dialing and may already serve the successor incarnation!
            let mut lanes = write(&self.lanes);
            if lanes
                .get(&peer.addr())
                .is_some_and(|lane| lane.peer == Some(peer))
            {
                lanes.remove(&peer.addr());
            }
        }

        self.liveness.quiesce(peer.incarnation());
        self.pending_replies.fail_peer(peer);

        for (target, watchers) in self.watchers.take_peer(peer) {
            for watcher in watchers {
                if let Err(error) = watcher.send_terminated(target) {
                    debug!(watcher_id = %watcher.watcher_id(), other_id = %target, %error, "cannot send synthesized terminated signal");
                }
            }
        }

        self.remove_wire_watches(peer);
        self.liveness.untrack(peer.incarnation());
    }

    /// Neither tombstoned nor severed: nothing here was promised anything about this peer, so it
    /// stays reachable and free to reconnect, re-sending its watches as it does.
    pub(crate) fn untrack_peer(&self, peer: NodeId) {
        self.remove_wire_watches(peer);
        self.liveness.untrack(peer.incarnation());
    }

    pub(crate) fn has_watches_involving(&self, addr: SocketAddr) -> bool {
        self.heartbeated_peers()
            .keys()
            .any(|peer| peer.addr() == addr)
    }

    pub(crate) fn heartbeated_peers(&self) -> HashMap<NodeId, PeerRole> {
        let mut peers = HashMap::new();
        for peer in self.watchers.peers() {
            peers.insert(peer, PeerRole::WatchedFromHere);
        }
        for peer in self.wire_watches.peers() {
            peers.entry(peer).or_insert(PeerRole::WatchingHere);
        }
        peers
    }

    /// The kept peers are reread inside the retention rather than taken from the tick's snapshot:
    /// a watch registered after the snapshot must not have its freshly tracked peer deleted, or
    /// failure detection for it would be disarmed for good.
    pub(crate) fn untrack_idle_peers(&self) {
        self.liveness.retain_with(|| {
            self.heartbeated_peers()
                .keys()
                .map(|peer| peer.incarnation())
                .collect()
        });
    }

    /// The tombstone is read under the lanes lock: read before it, a node death in between would
    /// find this lane unbound, skip it, and leave it bound to the dead incarnation, failing sends
    /// to the successor until the dead connection breaks.
    pub(crate) fn bind_lane(&self, addr: SocketAddr, lane_id: Generation, peer: NodeId) {
        let mut lanes = write(&self.lanes);
        let tombstoned = self.tombstoned(peer.incarnation());
        Self::bind_or_remove_lane(&mut lanes, addr, lane_id, peer, tombstoned);
    }

    /// Two readers for one peer could deliver a frame buffered on the dead connection behind one
    /// from the new connection. The lock is held from the old reader's shutdown, through awaiting
    /// its completion, to the new one's spawn: per address there is never a moment with two
    /// readers delivering.
    pub(crate) async fn supersede_inbound_reader<F>(&self, addr: SocketAddr, spawn_reader: F)
    where
        F: FnOnce(watch::Receiver<()>) -> JoinHandle<()>,
    {
        let mut readers = self.inbound_readers.lock().await;

        if let Some(superseded) = readers.remove(&addr) {
            let _ = superseded.shutdown_tx.send(());
            if let Err(error) = superseded.reader.await {
                warn!(peer_addr = %addr, %error, "superseded inbound reader panicked");
            }
        }

        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let reader = spawn_reader(shutdown_rx);
        readers.insert(
            addr,
            InboundReader {
                shutdown_tx,
                reader,
            },
        );
    }

    pub(crate) fn sever_rx(&self) -> watch::Receiver<u64> {
        self.sever_tx.subscribe()
    }

    #[cfg(feature = "remote-dev")]
    pub(crate) fn sever(&self) {
        self.sever_tx.send_modify(|generation| *generation += 1);
    }

    #[cfg(feature = "remote-dev")]
    pub(crate) fn arm_terminated_drop(&self, count: u64) {
        self.dropped_terminated.store(count, Ordering::Relaxed);
    }

    pub(crate) fn is_lane_open(&self, addr: SocketAddr, lane_id: Generation) -> bool {
        read(&self.lanes)
            .get(&addr)
            .is_some_and(|lane| lane.id == lane_id)
    }

    /// `true` if this call removed the lane, i.e. it was still the address's current one: a lane
    /// already removed, e.g. by [EndpointInner::node_death], may have a successor which now owns
    /// the address's pending lookups.
    pub(crate) fn remove_lane(&self, addr: SocketAddr, lane_id: Generation) -> bool {
        let mut lanes = write(&self.lanes);
        if lanes.get(&addr).is_some_and(|lane| lane.id == lane_id) {
            lanes.remove(&addr);
            true
        } else {
            false
        }
    }

    fn is_refused(&self, addr: SocketAddr) -> bool {
        matches!(lock(&self.addr_states).get(&addr), Some(AddrState::Refused))
    }

    /// A dead peer's lane is removed rather than bound, so a send to the successor incarnation
    /// opens a fresh lane instead of being refused; a lane with another ID belongs to a successor
    /// dial and stays untouched.
    fn bind_or_remove_lane(
        lanes: &mut HashMap<SocketAddr, Lane>,
        addr: SocketAddr,
        lane_id: Generation,
        peer: NodeId,
        tombstoned: bool,
    ) {
        if let Entry::Occupied(mut lane) = lanes.entry(addr)
            && lane.get().id == lane_id
        {
            if tombstoned {
                lane.remove();
            } else {
                lane.get_mut().peer = Some(peer);
            }
        }
    }

    fn remove_wire_watches(&self, peer: NodeId) {
        for watch in self.wire_watches.take_peer(peer) {
            if let Some(watcher_registry) = self.registry.watcher_registry(watch.target) {
                watcher_registry.remove(watch.wire_watcher_id);
            }
        }
    }

    fn try_send(
        outbound_tx: &CountedSender<Frame<'static>>,
        frame: Frame<'static>,
        peer: NodeId,
    ) -> Result<(), LaneError> {
        let result = if frame.is_message() {
            outbound_tx.try_send_counted(frame)
        } else {
            outbound_tx
                .try_send_uncounted(frame)
                .map_err(CountedSendError::from)
        };

        result.map_err(|error| match error {
            CountedSendError::Full(_) => LaneError::OutboundQueueFull(peer),
            CountedSendError::Disconnected(_) => LaneError::NodeUnreachable(peer),
        })
    }

    fn matching_lane<'a>(
        lanes: &'a HashMap<SocketAddr, Lane>,
        peer: NodeId,
        frame: &Frame,
    ) -> Result<Option<&'a CountedSender<Frame<'static>>>, LaneError> {
        match lanes.get(&peer.addr()) {
            Some(lane) if lane.peer.is_none_or(|bound| bound == peer) => {
                Ok(Some(lane.outbound_tx(frame)))
            }

            Some(_) => Err(LaneError::NodeUnreachable(peer)),

            None => Ok(None),
        }
    }

    /// One [Quota] for the whole lane, so the outbound capacity keeps its meaning however many
    /// streams share it.
    fn open_lane<'a>(
        &self,
        lanes: &'a mut HashMap<SocketAddr, Lane>,
        addr: SocketAddr,
        frame: &Frame,
    ) -> &'a CountedSender<Frame<'static>> {
        let quota = Quota::bounded(self.config.outbound_capacity);
        let lane_id = self.next_generation();

        let (control_tx, control_rx) = flume::unbounded();
        let control_tx = CountedSender::new(control_tx, quota.clone());
        let (data_tx, data_rx) = (0..self.data_streams)
            .map(|_| {
                let (data_tx, data_rx) = flume::unbounded();
                (CountedSender::new(data_tx, quota.clone()), data_rx)
            })
            .unzip::<_, _, Vec<_>, Vec<_>>();

        let lane = Lane {
            id: lane_id,
            peer: None,
            control_tx,
            data_tx,
        };
        lanes.insert(addr, lane);

        let dial_request = DialRequest {
            addr,
            lane_id,
            control_rx,
            data_rx,
            quota,
        };
        if self.dial_request_tx.send(dial_request).is_err() {
            error!(peer_addr = %addr, "dial loop gone, lane will never be connected");
            debug_assert!(false, "dial loop gone");
        }

        lanes
            .get(&addr)
            .expect("lane was just inserted")
            .outbound_tx(frame)
    }

    fn next_generation(&self) -> Generation {
        Generation(self.next_generation.fetch_add(1, Ordering::Relaxed))
    }

    #[cfg(feature = "remote-dev")]
    fn drop_terminated(&self) -> bool {
        self.dropped_terminated
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count.checked_sub(1)
            })
            .is_ok()
    }
}

pub(crate) fn get() -> Option<&'static EndpointInner> {
    ENDPOINT.get()
}

struct Lane {
    id: Generation,
    peer: Option<NodeId>,
    control_tx: CountedSender<Frame<'static>>,
    data_tx: Vec<CountedSender<Frame<'static>>>,
}

impl Lane {
    fn outbound_tx(&self, frame: &Frame) -> &CountedSender<Frame<'static>> {
        let data_tx = frame
            .recipient()
            .zip(NonZeroUsize::new(self.data_tx.len()))
            .map(|(recipient, streams)| &self.data_tx[stream_index(recipient, streams)]);

        data_tx.unwrap_or(&self.control_tx)
    }
}

struct InboundReader {
    shutdown_tx: watch::Sender<()>,
    reader: JoinHandle<()>,
}

enum AddrState {
    Refused,
    Known(NodeId),
}

/// The mapping only has to agree with itself: the receiver dispatches by the target named in the
/// frame, so nothing about it travels the wire.
fn stream_index(recipient: ActorId, streams: NonZeroUsize) -> usize {
    let mut hasher = DefaultHasher::new();
    recipient.hash(&mut hasher);
    hasher.finish() as usize % streams
}

/// A watch registered just after the snapshot which chose the lighter path is covered by the
/// next tick, which finds the node watched and declares it dead properly.
async fn liveness_loop(endpoint: &'static EndpointInner) {
    let mut ticks = interval(endpoint.config.heartbeat_interval);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        ticks.tick().await;

        let peers = endpoint.heartbeated_peers();
        for (peer, role) in &peers {
            if endpoint.tombstoned(peer.incarnation()) {
                continue;
            }

            if let Err(error) = endpoint.send(*peer, Frame::Ping) {
                debug!(%peer, %error, "cannot send heartbeat");
            }

            if !endpoint.liveness.available(peer.incarnation()) {
                match role {
                    PeerRole::WatchedFromHere => {
                        warn!(%peer, "node death, failure detection deadline exceeded");
                        endpoint.node_death(*peer);
                    }

                    PeerRole::WatchingHere => {
                        debug!(%peer, "untracking silent peer, no actor here watches it");
                        endpoint.untrack_peer(*peer);
                    }
                }
            }
        }

        endpoint.untrack_idle_peers();
    }
}

/// Re-asserting a watch is what compensates a `Terminated` frame lost with the watched side's
/// connection: the watched node answers a watch for a terminated actor right away, and no
/// reconnect on this side would otherwise ask again.
async fn watch_refresh_loop(endpoint: &'static EndpointInner) {
    let mut ticks = interval(endpoint.config.watch_refresh_interval);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        ticks.tick().await;

        for peer in endpoint.watchers.peers() {
            if endpoint.tombstoned(peer.incarnation()) {
                continue;
            }

            for (target, watcher) in endpoint.watchers.watches(peer) {
                if let Err(error) = endpoint.send(peer, Frame::Watch { target, watcher }) {
                    debug!(%peer, actor_id = %target, %error, "cannot refresh remote watch");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ActorId,
        quota::{CountedSender, Quota},
        remote::{
            endpoint::{EndpointInner, Generation, Lane, LaneError, stream_index},
            frame::Frame,
            node::NodeId,
        },
    };
    use flume::Receiver;
    use std::{borrow::Cow, collections::HashMap, num::NonZeroUsize};

    fn lane(
        streams: usize,
    ) -> (
        Lane,
        Receiver<Frame<'static>>,
        Vec<Receiver<Frame<'static>>>,
    ) {
        let quota = Quota::unbounded();

        let (control_tx, control_rx) = flume::unbounded();
        let control_tx = CountedSender::new(control_tx, quota.clone());
        let (data_tx, data_rx) = (0..streams)
            .map(|_| {
                let (data_tx, data_rx) = flume::unbounded();
                (CountedSender::new(data_tx, quota.clone()), data_rx)
            })
            .unzip::<_, _, Vec<_>, Vec<_>>();

        let lane = Lane {
            id: Generation(0),
            peer: None,
            control_tx,
            data_tx,
        };
        (lane, control_rx, data_rx)
    }

    fn send(lane: &Lane, frame: Frame<'static>) {
        lane.outbound_tx(&frame)
            .try_send_uncounted(frame)
            .expect("the queue is open");
    }

    /// A lane bound to one incarnation refuses frames for any other, so a frame for a dead
    /// incarnation never rides its successor's connection.
    #[test]
    fn a_frame_for_another_incarnation_is_refused() {
        let (mut lane, _control_rx, _data_rx) = lane(0);
        let addr = "127.0.0.1:2552".parse().expect("valid address");
        let peer = NodeId::new(addr);
        lane.peer = Some(peer);
        let lanes = HashMap::from([(addr, lane)]);

        assert!(matches!(
            EndpointInner::matching_lane(&lanes, peer, &Frame::Ping),
            Ok(Some(_))
        ));

        let successor = NodeId::new(addr);
        assert!(matches!(
            EndpointInner::matching_lane(&lanes, successor, &Frame::Ping),
            Err(LaneError::NodeUnreachable(_))
        ));

        let unknown = NodeId::new("127.0.0.1:2553".parse().expect("valid address"));
        assert!(matches!(
            EndpointInner::matching_lane(&lanes, unknown, &Frame::Ping),
            Ok(None)
        ));
    }

    /// A transport without data streams puts every frame on the control stream, which is one
    /// ordered lane per peer carrying everything: the guarantees hold there by the same argument,
    /// not as a special case.
    #[test]
    fn without_data_streams_everything_rides_the_control_stream() {
        let (lane, control_rx, data_rx) = lane(0);
        let (target, watcher) = (ActorId::new(), ActorId::new());

        send(
            &lane,
            Frame::Message {
                target,
                reply_tags: Vec::new(),
                payload: Cow::Borrowed(&[]),
            },
        );
        send(&lane, Frame::Terminated { target, watcher });
        send(&lane, Frame::Watch { target, watcher });
        send(&lane, Frame::Ping);

        assert!(data_rx.is_empty());
        assert_eq!(control_rx.len(), 4);
    }

    /// A terminated signal rides its watcher's stream, the one the messages to that watcher ride:
    /// that shared queue is the whole mechanism behind the ordering guarantee.
    #[test]
    fn a_terminated_signal_shares_its_watchers_stream() {
        let streams = NonZeroUsize::new(8).expect("8 is not zero");
        let (lane, control_rx, data_rx) = lane(streams.get());
        let watcher = ActorId::new();

        send(
            &lane,
            Frame::Message {
                target: watcher,
                reply_tags: Vec::new(),
                payload: Cow::Borrowed(&[]),
            },
        );
        send(
            &lane,
            Frame::Terminated {
                target: ActorId::new(),
                watcher,
            },
        );

        assert_eq!(data_rx[stream_index(watcher, streams)].len(), 2);
        assert!(control_rx.is_empty());
    }

    /// Per-node frames stay on the control stream even where data streams exist: only frames
    /// delivered to an actor have a stream to pick.
    #[test]
    fn per_node_frames_stay_on_the_control_stream() {
        let (lane, control_rx, data_rx) = lane(8);
        let (target, watcher) = (ActorId::new(), ActorId::new());

        send(&lane, Frame::Watch { target, watcher });
        send(&lane, Frame::Unwatch { target, watcher });
        send(&lane, Frame::Ping);

        assert_eq!(control_rx.len(), 3);
        assert!(data_rx.iter().all(Receiver::is_empty));
    }

    /// A dial which lost the race against node death removes its lane instead of binding it to
    /// the dead incarnation, so a send to the successor opens a fresh lane rather than being
    /// refused until the dead connection breaks.
    #[test]
    fn binding_a_tombstoned_peer_removes_the_lane() {
        let addr = "127.0.0.1:2552".parse().expect("valid address");
        let peer = NodeId::new(addr);
        let (lane, _control_rx, _data_rx) = lane(0);
        let mut lanes = HashMap::from([(addr, lane)]);

        EndpointInner::bind_or_remove_lane(&mut lanes, addr, Generation(0), peer, true);

        assert!(lanes.is_empty());
    }

    /// Binding a live peer names it on the lane, and a lane under another ID or address stays
    /// untouched either way: it belongs to a successor dial, which a stale task must not close.
    #[test]
    fn binding_names_the_peer_and_spares_other_lanes() {
        let addr = "127.0.0.1:2552".parse().expect("valid address");
        let peer = NodeId::new(addr);
        let (lane, _control_rx, _data_rx) = lane(0);
        let mut lanes = HashMap::from([(addr, lane)]);

        EndpointInner::bind_or_remove_lane(&mut lanes, addr, Generation(0), peer, false);
        assert_eq!(lanes.get(&addr).and_then(|lane| lane.peer), Some(peer));

        EndpointInner::bind_or_remove_lane(
            &mut lanes,
            addr,
            Generation(1),
            NodeId::new(addr),
            true,
        );
        assert_eq!(lanes.get(&addr).and_then(|lane| lane.peer), Some(peer));

        let other_addr = "127.0.0.1:2553".parse().expect("valid address");
        EndpointInner::bind_or_remove_lane(&mut lanes, other_addr, Generation(0), peer, true);
        assert_eq!(lanes.len(), 1);
    }
}
