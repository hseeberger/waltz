use crate::{
    Backoff,
    quota::{CountedSendError, CountedSender, Quota},
    remote::{
        codec::{Codec, Postcard},
        failure::{DeadlineFailureDetector, DeliveryGate, FailureDetector, Liveness},
        frame::Frame,
        node::{Incarnation, NodeId},
        peer::{DialRequest, accept_loop, dial_loop},
        registry::Registry,
        transport::Transport,
        watch::{WatcherTable, WireWatchTable},
    },
    sync::{lock, read, write},
};
use derive_more::Debug;
use flume::Sender;
use log::{debug, error, warn};
use std::{
    collections::{HashMap, HashSet},
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
    task::{self, AbortHandle},
    time::{MissedTickBehavior, interval},
};

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
    /// and are never dropped.
    pub outbound_capacity: NonZeroUsize,

    /// The maximum size of an encoded frame in bytes; larger inbound frames kill the connection.
    pub max_frame_size: NonZeroUsize,

    /// The codec encoding and decoding message payloads.
    #[debug(skip)]
    pub codec: Arc<dyn Codec>,

    /// The interval for `Ping` frames towards the nodes a watch names in either direction, i.e.
    /// those with watched actors and those watching actors here; also the tick of the failure
    /// detection.
    pub heartbeat_interval: Duration,

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
    /// 8192 messages, a maximum frame size of 1 MiB, the [Postcard] codec, a heartbeat interval
    /// of one second, a five second failure detection deadline and reconnect backoff from 250 ms
    /// to 3 s with at most 8 attempts for unwatched nodes.
    pub fn new(advertised_addr: SocketAddr) -> Self {
        Self {
            advertised_addr,
            outbound_capacity: const { NonZeroUsize::new(8_192).expect("8192 is not zero") },
            max_frame_size: const { NonZeroUsize::new(1_024 * 1_024).expect("1 MiB is not zero") },
            codec: Arc::new(Postcard),
            heartbeat_interval: Duration::from_secs(1),
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
        config,
        registry: Registry::new(),
        lanes: RwLock::new(HashMap::new()),
        inbound_readers: Mutex::new(HashMap::new()),
        next_generation: AtomicU64::new(0),
        tombstones: RwLock::new(HashSet::new()),
        addr_states: Mutex::new(HashMap::new()),
        liveness: Liveness::new(),
        watchers: WatcherTable::new(),
        wire_watches: WireWatchTable::new(),
        dial_request_tx,
    };
    ENDPOINT
        .set(inner)
        .map_err(|_| StartError::AlreadyStarted)?;
    let endpoint = ENDPOINT.get().expect("endpoint was just set");

    let transport = Arc::new(transport);
    task::spawn(accept_loop(transport.clone(), endpoint));
    task::spawn(dial_loop(transport, dial_request_rx, endpoint));
    task::spawn(liveness_loop(endpoint));
    Ok(())
}

/// A task and the entry it was started for must agree, so a stale task cannot close a newer one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Generation(u64);

#[derive(Debug, Error)]
pub(crate) enum LaneError {
    #[error("node {0} unreachable")]
    NodeUnreachable(NodeId),

    #[error("outbound queue towards node {0} full")]
    OutboundQueueFull(NodeId),
}

pub(crate) struct EndpointInner {
    node: NodeId,
    config: EndpointConfig,
    registry: Registry,
    lanes: RwLock<HashMap<SocketAddr, Lane>>,
    inbound_readers: Mutex<HashMap<SocketAddr, InboundReader>>,
    next_generation: AtomicU64,
    tombstones: RwLock<HashSet<Incarnation>>,
    addr_states: Mutex<HashMap<SocketAddr, AddrState>>,
    liveness: Liveness,
    watchers: WatcherTable,
    wire_watches: WireWatchTable,
    dial_request_tx: Sender<DialRequest>,
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

    pub(crate) fn registry(&self) -> &Registry {
        &self.registry
    }

    pub(crate) fn watchers(&self) -> &WatcherTable {
        &self.watchers
    }

    pub(crate) fn wire_watches(&self) -> &WireWatchTable {
        &self.wire_watches
    }

    pub(crate) fn reconnect_backoff(&self, attempts: u32) -> Duration {
        self.config
            .reconnect_backoff
            .duration(attempts.saturating_sub(1))
    }

    /// Message frames are subject to the outbound capacity, system frames bypass it; both ride the
    /// same FIFO lane, which is dialed on first use.
    ///
    /// A lane belongs to an address but serves one incarnation: once [EndpointInner::bind_lane] has
    /// named the handshaked peer, a frame for any other incarnation there is refused rather than
    /// written onto its successor's connection.
    pub(crate) fn send(&self, peer: NodeId, frame: Frame) -> Result<(), LaneError> {
        if self.tombstoned(peer.incarnation()) {
            return Err(LaneError::NodeUnreachable(peer));
        }

        let outbound_tx = self.lane_outbound_tx(peer)?;

        if frame.is_message() {
            outbound_tx
                .try_send_counted(frame)
                .map_err(|error| match error {
                    CountedSendError::Full(_) => LaneError::OutboundQueueFull(peer),
                    CountedSendError::Disconnected(_) => LaneError::NodeUnreachable(peer),
                })
        } else {
            outbound_tx
                .try_send_uncounted(frame)
                .map_err(|_| LaneError::NodeUnreachable(peer))
        }
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
            warn!(peer:% = peer, previous:% = previous; "node death, address reused by a new incarnation");
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

    /// Tombstone, close the lane, wait out in flight deliveries, only then flush the synthesized
    /// signals: this order makes the contract true by construction, i.e. after the signal no
    /// message from that node is ever delivered again.
    ///
    /// Only for a node an actor here watches: the tombstone is permanent, which is the price of
    /// backing a synthesized signal. A silent peer nothing watches goes to
    /// [EndpointInner::untrack_peer] instead.
    pub(crate) fn node_death(&self, peer: NodeId) {
        if !write(&self.tombstones).insert(peer.incarnation()) {
            return;
        }

        {
            let mut lanes = write(&self.lanes);
            if lanes
                .get(&peer.addr())
                .is_some_and(|lane| lane.peer.is_none_or(|bound| bound == peer))
            {
                lanes.remove(&peer.addr());
            }
        }

        self.liveness.quiesce(peer.incarnation());

        for (target, watchers) in self.watchers.take_peer(peer) {
            for watcher in watchers {
                if let Err(error) = watcher.send_terminated(target) {
                    debug!(watcher_id:% = watcher.watcher_id(), other_id:% = target, error:%; "cannot send synthesized terminated signal");
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

    /// The flag tells whether an actor here watches the peer, i.e. whether its silence means node
    /// death rather than mere untracking.
    pub(crate) fn heartbeated_peers(&self) -> HashMap<NodeId, bool> {
        let mut peers = HashMap::new();
        for peer in self.watchers.peers() {
            peers.insert(peer, true);
        }
        for peer in self.wire_watches.peers() {
            peers.entry(peer).or_insert(false);
        }
        peers
    }

    pub(crate) fn untrack_idle_peers(&self, heartbeated: &HashMap<NodeId, bool>) {
        let keep = heartbeated
            .keys()
            .map(|peer| peer.incarnation())
            .collect::<HashSet<_>>();

        self.liveness.retain(&keep);
    }

    pub(crate) fn bind_lane(&self, addr: SocketAddr, lane_id: Generation, peer: NodeId) {
        if let Some(lane) = write(&self.lanes).get_mut(&addr)
            && lane.id == lane_id
        {
            lane.peer = Some(peer);
        }
    }

    /// Two readers for one peer could deliver a frame buffered on the dead connection behind one
    /// from the new connection. Aborting is safe exactly because a reader never holds the delivery
    /// gate across an await.
    pub(crate) fn supersede_inbound_reader(
        &self,
        addr: SocketAddr,
        abort: AbortHandle,
    ) -> Generation {
        let id = self.next_generation();

        let superseded = lock(&self.inbound_readers).insert(addr, InboundReader { id, abort });
        if let Some(superseded) = superseded {
            superseded.abort.abort();
        }

        id
    }

    pub(crate) fn remove_inbound_reader(&self, addr: SocketAddr, reader_id: Generation) {
        let mut readers = lock(&self.inbound_readers);
        if readers
            .get(&addr)
            .is_some_and(|reader| reader.id == reader_id)
        {
            readers.remove(&addr);
        }
    }

    pub(crate) fn is_lane_open(&self, addr: SocketAddr, lane_id: Generation) -> bool {
        read(&self.lanes)
            .get(&addr)
            .is_some_and(|lane| lane.id == lane_id)
    }

    pub(crate) fn remove_lane(&self, addr: SocketAddr, lane_id: Generation) {
        let mut lanes = write(&self.lanes);
        if lanes.get(&addr).is_some_and(|lane| lane.id == lane_id) {
            lanes.remove(&addr);
        }
    }

    fn is_refused(&self, addr: SocketAddr) -> bool {
        matches!(lock(&self.addr_states).get(&addr), Some(AddrState::Refused))
    }

    fn remove_wire_watches(&self, peer: NodeId) {
        for watch in self.wire_watches.take_peer(peer) {
            if let Some(watcher_registry) = self.registry.watcher_registry(watch.target) {
                watcher_registry.remove(watch.wire_watcher_id);
            }
        }
    }

    /// The steady state takes only a read lock, so senders towards different peers do not
    /// serialize; the opening path has to look again under the write lock, since another sender
    /// may have opened the lane in between.
    fn lane_outbound_tx(&self, peer: NodeId) -> Result<CountedSender<Frame>, LaneError> {
        let open = Self::matching_lane(&read(&self.lanes), peer)?;
        if let Some(outbound_tx) = open {
            return Ok(outbound_tx);
        }

        let mut lanes = write(&self.lanes);
        if let Some(outbound_tx) = Self::matching_lane(&lanes, peer)? {
            return Ok(outbound_tx);
        }
        if self.is_refused(peer.addr()) {
            return Err(LaneError::NodeUnreachable(peer));
        }

        Ok(self.open_lane(&mut lanes, peer.addr()))
    }

    fn matching_lane(
        lanes: &HashMap<SocketAddr, Lane>,
        peer: NodeId,
    ) -> Result<Option<CountedSender<Frame>>, LaneError> {
        match lanes.get(&peer.addr()) {
            Some(lane) if lane.peer.is_none_or(|bound| bound == peer) => {
                Ok(Some(lane.outbound_tx.clone()))
            }

            Some(_) => Err(LaneError::NodeUnreachable(peer)),

            None => Ok(None),
        }
    }

    fn open_lane(
        &self,
        lanes: &mut HashMap<SocketAddr, Lane>,
        addr: SocketAddr,
    ) -> CountedSender<Frame> {
        let (outbound_tx, outbound_rx) = flume::unbounded();
        let quota = Quota::bounded(self.config.outbound_capacity);
        let outbound_tx = CountedSender::new(outbound_tx, quota.clone());
        let lane_id = self.next_generation();

        lanes.insert(
            addr,
            Lane {
                id: lane_id,
                peer: None,
                outbound_tx: outbound_tx.clone(),
            },
        );
        let dial_request = DialRequest {
            addr,
            lane_id,
            outbound_rx,
            quota,
        };
        if self.dial_request_tx.send(dial_request).is_err() {
            error!(peer_addr:% = addr; "dial loop gone, lane will never be connected");
            debug_assert!(false, "dial loop gone");
        }

        outbound_tx
    }

    fn next_generation(&self) -> Generation {
        Generation(self.next_generation.fetch_add(1, Ordering::Relaxed))
    }
}

pub(crate) fn get() -> Option<&'static EndpointInner> {
    ENDPOINT.get()
}

struct Lane {
    id: Generation,
    peer: Option<NodeId>,
    outbound_tx: CountedSender<Frame>,
}

struct InboundReader {
    id: Generation,
    abort: AbortHandle,
}

enum AddrState {
    Refused,
    Known(NodeId),
}

/// A watch registered just after the snapshot which chose the lighter path is covered by the
/// next tick, which finds the node watched and declares it dead properly.
async fn liveness_loop(endpoint: &'static EndpointInner) {
    let mut ticks = interval(endpoint.config.heartbeat_interval);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        ticks.tick().await;

        let peers = endpoint.heartbeated_peers();
        for (peer, watched) in &peers {
            if endpoint.tombstoned(peer.incarnation()) {
                continue;
            }

            if let Err(error) = endpoint.send(*peer, Frame::Ping) {
                debug!(peer:% = peer, error:%; "cannot send heartbeat");
            }

            if !endpoint.liveness.available(peer.incarnation()) {
                if *watched {
                    warn!(peer:% = peer; "node death, failure detection deadline exceeded");
                    endpoint.node_death(*peer);
                } else {
                    debug!(peer:% = peer; "untracking silent peer, no actor here watches it");
                    endpoint.untrack_peer(*peer);
                }
            }
        }

        endpoint.untrack_idle_peers(&peers);
    }
}
