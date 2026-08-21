use crate::{
    quota::Quota,
    remote::{
        discovery,
        endpoint::{EndpointInner, Generation},
        failure::DeliveryGate,
        frame::{Frame, Handshake, HandshakeError},
        node::NodeId,
        reply,
        transport::{
            ConnectedControl, Connection, FrameReceiver, FrameSender, Transport, TransportError,
        },
        watch,
    },
};
use flume::Receiver;
use std::{iter, net::SocketAddr, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::{
    select,
    task::{self, JoinSet},
    time::{Instant, sleep, timeout},
};
use tracing::{debug, error, warn};

pub(crate) struct DialRequest {
    pub(crate) addr: SocketAddr,
    pub(crate) lane_id: Generation,
    pub(crate) control_rx: Receiver<Frame<'static>>,
    pub(crate) data_rx: Vec<Receiver<Frame<'static>>>,
    pub(crate) quota: Quota,
}

/// A transport failure or a silent peer is transient and worth another attempt, everything else
/// says the peer does not speak this protocol and never will.
#[derive(Debug, Error)]
pub(crate) enum ConnectError {
    #[error(transparent)]
    Transport(#[from] TransportError),

    #[error("connection closed before handshake")]
    Closed,

    #[error("timed out opening {0} data streams")]
    DataStreams(usize),

    #[error("timed out exchanging handshakes")]
    HandshakeTimeout,

    #[error("first frame is not a handshake")]
    NotAHandshake,

    #[error("cannot decode handshake frame")]
    Decode(#[from] postcard::Error),

    #[error(transparent)]
    Handshake(#[from] HandshakeError),
}

impl ConnectError {
    fn is_retryable(&self) -> bool {
        match self {
            ConnectError::Transport(TransportError::FrameTooLarge { .. }) => false,

            ConnectError::Transport(_)
            | ConnectError::Closed
            | ConnectError::DataStreams(_)
            | ConnectError::HandshakeTimeout => true,

            ConnectError::NotAHandshake | ConnectError::Decode(_) | ConnectError::Handshake(_) => {
                false
            }
        }
    }
}

pub(crate) async fn accept_loop<T>(transport: Arc<T>, endpoint: &'static EndpointInner)
where
    T: Transport,
{
    loop {
        match transport
            .accept(endpoint.config().max_frame_size.get())
            .await
        {
            Ok(connection) => {
                task::spawn(run_accepted(connection, endpoint));
            }

            Err(error) => {
                error!(%error, "remoting endpoint cannot accept connections");
                break;
            }
        }
    }
}

pub(crate) async fn dial_loop<T>(
    transport: Arc<T>,
    dial_request_rx: Receiver<DialRequest>,
    endpoint: &'static EndpointInner,
) where
    T: Transport,
{
    while let Ok(request) = dial_request_rx.recv_async().await {
        task::spawn(run_peer(transport.clone(), request, endpoint));
    }
}

/// The handshake is bounded by the heartbeat interval, mirroring [open_data_streams]: an
/// unauthenticated dialer which connects and never speaks must not pin this task forever.
async fn run_accepted<C>(connection: C, endpoint: &'static EndpointInner)
where
    C: Connection,
{
    let establishing = timeout(endpoint.config().heartbeat_interval, async {
        let (mut frame_sender, mut frame_receiver) = match connection.accept_control().await {
            Ok(halves) => halves,
            Err(error) => {
                warn!(%error, "cannot open inbound connection");
                return None;
            }
        };

        let peer = match recv_handshake(&mut frame_receiver).await {
            Ok(peer) => peer,
            Err(error) => {
                warn!(%error, "cannot receive inbound handshake");
                return None;
            }
        };
        if let Err(error) = send_handshake(&mut frame_sender, endpoint.node()).await {
            warn!(%peer, %error, "cannot send outbound handshake");
            return None;
        }

        Some((frame_sender, frame_receiver, peer))
    })
    .await;

    let established = match establishing {
        Ok(established) => established,

        Err(_) => {
            debug!("timed out establishing inbound connection");
            return;
        }
    };
    let Some((frame_sender, frame_receiver, peer)) = established else {
        return;
    };

    let Some(gate) = admit(peer, endpoint) else {
        return;
    };
    debug!(%peer, "inbound connection established");

    endpoint
        .supersede_inbound_reader(peer.addr(), |shutdown_rx| {
            task::spawn(read_streams(
                connection,
                frame_receiver,
                frame_sender,
                peer,
                gate,
                endpoint,
                shutdown_rx,
            ))
        })
        .await;
}

/// Every stream of a connection is read by its own task, all owned by one [JoinSet], which is
/// shut down with its tasks awaited on every exit: a successor reader must never overlap with a
/// delivery still draining here. A poisoned stream ends the whole connection, else its recipients
/// would stall silently until another frame happened to ride it.
async fn read_streams<C>(
    connection: C,
    control_rx: C::Receiver,
    frame_sender: C::Sender,
    peer: NodeId,
    gate: DeliveryGate,
    endpoint: &'static EndpointInner,
    mut shutdown_rx: tokio::sync::watch::Receiver<()>,
) where
    C: Connection,
{
    let mut sever_rx = endpoint.sever_rx();

    let mut readers = JoinSet::new();
    readers.spawn(read_frames(control_rx, peer, gate.clone(), endpoint));

    loop {
        select! {
            biased;

            _ = shutdown_rx.changed() => break,

            _ = sever_rx.changed() => break,

            joined = readers.join_next() => match joined {
                Some(Ok(ReadEnd::Poisoned)) | None => break,
                Some(_) => {}
            },

            accepted = connection.accept_data() => match accepted {
                Ok(Some(data_rx)) => {
                    readers.spawn(read_frames(data_rx, peer, gate.clone(), endpoint));
                }

                Ok(None) => break,

                Err(error) => {
                    debug!(%peer, %error, "no further inbound stream");
                    break;
                }
            },
        }
    }

    readers.shutdown().await;
    drop(frame_sender);
}

/// A connection's reader must be aborted and awaited before the next one is dialed: two readers
/// for one peer could deliver a frame buffered on the dead connection behind one from the new
/// connection, and an abort alone leaves the old reader draining for a moment longer.
///
/// Watches are re-sent on every connection, since a lane opened again for an already watched peer
/// carries no `Watch` of its own; pending lookups only on a reconnect, since a lane which dies
/// fails them rather than leaving them for its successor.
async fn run_peer<T>(transport: Arc<T>, request: DialRequest, endpoint: &'static EndpointInner)
where
    T: Transport,
{
    let DialRequest {
        addr,
        lane_id,
        control_rx,
        data_rx,
        quota,
    } = request;
    let streams = data_rx.len();
    let outbound_rx = iter::once(control_rx).chain(data_rx).collect::<Vec<_>>();
    let mut sever_rx = endpoint.sever_rx();
    let mut attempts = 0u32;
    let mut reconnected = false;

    loop {
        match connect(transport.as_ref(), addr, streams, endpoint).await {
            Ok(connected) => {
                let Connected {
                    frame_senders,
                    frame_receiver,
                    peer,
                } = connected;
                let Some(gate) = admit(peer, endpoint) else {
                    break;
                };
                let reader = task::spawn(read_frames(frame_receiver, peer, gate, endpoint));

                endpoint.bind_lane(addr, lane_id, peer);
                let connected_at = Instant::now();
                debug!(%peer, "outbound connection established");

                // Watch frames lost with a connection must be re-sent here; idempotent remotely.
                for (target, watcher) in endpoint.watchers().watches(peer) {
                    if let Err(error) = endpoint.send(peer, Frame::Watch { target, watcher }) {
                        warn!(%peer, actor_id = %target, %error, "cannot re-establish remote watch");
                    }
                }
                if reconnected {
                    for frame in endpoint.pending_lookups().frames(addr) {
                        if let Err(error) = endpoint.send_to_addr(addr, frame) {
                            warn!(peer_addr = %addr, %error, "cannot re-send lookup");
                        }
                    }
                }
                reconnected = true;

                let end = select! {
                    end = write_streams(
                        frame_senders,
                        &outbound_rx,
                        &quota,
                        addr,
                        endpoint.config().max_frame_size.get(),
                    ) => end,

                    _ = sever_rx.changed() => WriterEnd::ConnectionLost,
                };
                reader.abort();
                let _ = reader.await;

                if matches!(end, WriterEnd::LaneClosed) {
                    break;
                }

                // A connection lost before proving itself must meet the backoff, not a hot redial.
                if connected_at.elapsed() >= endpoint.config().heartbeat_interval {
                    attempts = 0;
                } else {
                    attempts += 1;

                    if !backoff_or_give_up(endpoint, addr, attempts).await {
                        break;
                    }
                }
            }

            Err(error) if !error.is_retryable() => {
                warn!(peer_addr = %addr, %error, "giving up connecting to node, not a waltz node of this protocol version");
                endpoint.refuse(addr);
                break;
            }

            Err(error) => {
                attempts += 1;
                debug!(peer_addr = %addr, attempts, %error, "cannot connect to node");

                if !backoff_or_give_up(endpoint, addr, attempts).await {
                    break;
                }
            }
        }

        if !endpoint.is_lane_open(addr, lane_id) {
            break;
        }
    }

    if endpoint.remove_lane(addr, lane_id) {
        endpoint.pending_lookups().fail(addr);
        endpoint.pending_replies().fail_addr(addr);
    }
    for outbound_rx in outbound_rx {
        drain_dead_letters(outbound_rx, addr).await;
    }
}

/// One decision, two callers: a peer involved in a watch is retried forever, since failure
/// detection, not the attempt count, settles its fate.
async fn backoff_or_give_up(endpoint: &EndpointInner, addr: SocketAddr, attempts: u32) -> bool {
    if !endpoint.has_watches_involving(addr) && attempts >= endpoint.config().max_connect_attempts {
        warn!(peer_addr = %addr, "giving up connecting to node");
        return false;
    }

    sleep(endpoint.reconnect_backoff(attempts)).await;
    true
}

/// The control stream comes first, the data streams in the order their queues were created, which
/// is the order [EndpointInner::send] indexes them by.
struct Connected<C>
where
    C: Connection,
{
    frame_senders: Vec<C::Sender>,
    frame_receiver: C::Receiver,
    peer: NodeId,
}

async fn connect<T>(
    transport: &T,
    addr: SocketAddr,
    streams: usize,
    endpoint: &EndpointInner,
) -> Result<Connected<T::Connection>, ConnectError>
where
    T: Transport,
{
    let ConnectedControl {
        connection,
        mut control_tx,
        control_rx: mut frame_receiver,
    } = transport
        .connect(addr, endpoint.config().max_frame_size.get())
        .await?;

    let peer = exchange_handshakes(
        &mut control_tx,
        &mut frame_receiver,
        endpoint.node(),
        endpoint.config().heartbeat_interval,
    )
    .await?;

    let mut frame_senders = vec![control_tx];
    frame_senders.extend(open_data_streams(&connection, streams, endpoint).await?);

    Ok(Connected {
        frame_senders,
        frame_receiver,
        peer,
    })
}

/// Bounded like the accept side's establishment: an authenticated peer which connects and never
/// sends its handshake must not pin this lane's dial forever.
async fn exchange_handshakes<S, R>(
    frame_sender: &mut S,
    frame_receiver: &mut R,
    node: NodeId,
    within: Duration,
) -> Result<NodeId, ConnectError>
where
    R: FrameReceiver,
    S: FrameSender,
{
    let exchange = async {
        send_handshake(frame_sender, node).await?;
        recv_handshake(frame_receiver).await
    };

    match timeout(within, exchange).await {
        Ok(peer) => peer,
        Err(_) => Err(ConnectError::HandshakeTimeout),
    }
}

/// Opened before the lane carries anything, so a peer admitting fewer concurrent streams fails
/// the connection here, into the reconnect path, rather than stalling whichever queue hashes onto
/// a stream that was never granted; the heartbeat interval bounds the wait, since a peer silent
/// for longer is one this endpoint gives up on anyway.
async fn open_data_streams<C>(
    connection: &C,
    streams: usize,
    endpoint: &EndpointInner,
) -> Result<Vec<C::Sender>, ConnectError>
where
    C: Connection,
{
    let open = async {
        let mut frame_senders = Vec::with_capacity(streams);
        for _ in 0..streams {
            frame_senders.push(connection.open_data().await?);
        }
        Ok::<_, ConnectError>(frame_senders)
    };

    match timeout(endpoint.config().heartbeat_interval, open).await {
        Ok(frame_senders) => frame_senders,

        Err(_) => Err(ConnectError::DataStreams(streams)),
    }
}

enum WriterEnd {
    LaneClosed,
    ConnectionLost,
}

/// [ReadEnd::Poisoned] demands the connection's end, [ReadEnd::Closed] merely reports this
/// stream's.
enum ReadEnd {
    Closed,
    Poisoned,
}

/// One writer per stream, all sharing the lane's quota; the first one to end ends them all, since
/// a lost connection is lost for every stream and the queues are written on the next one.
async fn write_streams<S>(
    frame_senders: Vec<S>,
    outbound_rx: &[Receiver<Frame<'static>>],
    quota: &Quota,
    addr: SocketAddr,
    max_frame_size: usize,
) -> WriterEnd
where
    S: FrameSender,
{
    debug_assert_eq!(
        frame_senders.len(),
        outbound_rx.len(),
        "one writer per queue"
    );

    let mut writers = JoinSet::new();
    for (frame_sender, outbound_rx) in frame_senders.into_iter().zip(outbound_rx) {
        writers.spawn(write_frames(
            frame_sender,
            outbound_rx.clone(),
            quota.clone(),
            addr,
            max_frame_size,
        ));
    }

    match writers.join_next().await {
        Some(Ok(end)) => end,
        Some(Err(_)) | None => WriterEnd::ConnectionLost,
    }
}

/// The reservation is released on dequeue, before the send is awaited: a writer aborted mid send
/// must not leave the quota short of a slot for the life of the lane.
async fn write_frames<S>(
    mut frame_sender: S,
    outbound_rx: Receiver<Frame<'static>>,
    quota: Quota,
    addr: SocketAddr,
    max_frame_size: usize,
) -> WriterEnd
where
    S: FrameSender,
{
    let mut buffer = Vec::new();

    loop {
        let Ok(frame) = outbound_rx.recv_async().await else {
            return WriterEnd::LaneClosed;
        };
        if frame.is_message() {
            quota.unreserve();
        }

        buffer = match frame.encode_into(buffer) {
            Ok(bytes) => bytes,

            Err(error) => {
                warn!(peer_addr = %addr, %error, "cannot encode frame");
                buffer = Vec::new();
                continue;
            }
        };

        // An oversize frame must never reach the transport: the receiver's refusal kills the
        // connection!
        if buffer.len() > max_frame_size {
            oversize_dead_letter(&frame, addr);
            continue;
        }

        if let Err(error) = frame_sender.send(&buffer).await {
            warn!(peer_addr = %addr, %error, "connection lost");
            dead_letter(frame, addr);
            return WriterEnd::ConnectionLost;
        }
    }
}

/// In arrival order, each delivery holding the peer's gate and rechecking the tombstone: once
/// the node death sequence has flushed its signals, no further frame from that incarnation is
/// delivered.
async fn read_frames<R>(
    mut frame_receiver: R,
    peer: NodeId,
    gate: DeliveryGate,
    endpoint: &'static EndpointInner,
) -> ReadEnd
where
    R: FrameReceiver,
{
    loop {
        let bytes = match frame_receiver.recv().await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                debug!(%peer, "connection closed by peer");
                return ReadEnd::Closed;
            }
            Err(error) => {
                debug!(%peer, %error, "connection lost");
                return ReadEnd::Closed;
            }
        };

        let frame = match Frame::from_bytes(bytes) {
            Ok(frame) => frame,
            Err(error) => {
                warn!(%peer, %error, "closing connection, cannot decode frame");
                return ReadEnd::Poisoned;
            }
        };

        endpoint.record_heartbeat(peer.incarnation());

        let _guard = gate.enter();
        if endpoint.tombstoned(peer.incarnation()) {
            debug!(%peer, "closing connection to a dead node incarnation");
            return ReadEnd::Poisoned;
        }

        match frame {
            Frame::Message {
                target,
                reply_tags,
                payload,
            } => {
                if let Err(error) = endpoint
                    .registry()
                    .deliver(target, &payload, endpoint.codec())
                {
                    warn!(%peer, actor_id = %target, %error, "dead letter");
                    reply::on_undeliverable(endpoint, peer, &reply_tags);
                }
            }

            Frame::Watch { target, watcher } => watch::on_watch(endpoint, peer, target, watcher),

            Frame::Unwatch { target, watcher } => {
                watch::on_unwatch(endpoint, peer, target, watcher)
            }

            Frame::Terminated { target, watcher } => {
                watch::on_terminated(endpoint, peer, target, watcher)
            }

            Frame::Lookup { nonce, key } => discovery::on_lookup(endpoint, peer, nonce, key),

            Frame::LookupReply { nonce, result } => {
                discovery::on_lookup_reply(endpoint, nonce, result)
            }

            Frame::Reply {
                nonce,
                recipient: _,
                payload,
            } => reply::on_reply(endpoint, peer, nonce, &payload),

            Frame::ReplyDropped {
                nonce,
                recipient: _,
            } => reply::on_reply_dropped(endpoint, nonce),

            Frame::Ping => {}

            Frame::Handshake(_) => {
                warn!(%peer, "closing connection, unexpected handshake frame");
                return ReadEnd::Poisoned;
            }
        }
    }
}

/// The tombstone check must come before [EndpointInner::note_peer]: refusing a dead
/// incarnation's handshake is what backs the synthesized signals.
fn admit(peer: NodeId, endpoint: &EndpointInner) -> Option<DeliveryGate> {
    if endpoint.tombstoned(peer.incarnation()) {
        warn!(%peer, "refusing connection, dead node incarnation");
        return None;
    }

    Some(endpoint.note_peer(peer))
}

async fn send_handshake<S>(frame_sender: &mut S, node: NodeId) -> Result<(), TransportError>
where
    S: FrameSender,
{
    let bytes = Frame::Handshake(Handshake::new(node))
        .encode_into(Vec::new())
        .map_err(TransportError::other)?;
    frame_sender.send(&bytes).await
}

async fn recv_handshake<R>(frame_receiver: &mut R) -> Result<NodeId, ConnectError>
where
    R: FrameReceiver,
{
    let bytes = frame_receiver.recv().await?.ok_or(ConnectError::Closed)?;

    match Frame::from_bytes(bytes)? {
        Frame::Handshake(handshake) => Ok(handshake.validate()?),
        _ => Err(ConnectError::NotAHandshake),
    }
}

async fn drain_dead_letters(outbound_rx: Receiver<Frame<'static>>, addr: SocketAddr) {
    while let Ok(frame) = outbound_rx.recv_async().await {
        dead_letter(frame, addr);
    }
}

fn dead_letter(frame: Frame<'static>, addr: SocketAddr) {
    if let Frame::Message { target, .. } = frame {
        warn!(actor_id = %target, peer_addr = %addr, "dead letter, node unreachable");
    }
}

fn oversize_dead_letter(frame: &Frame<'_>, addr: SocketAddr) {
    match frame {
        Frame::Message { target, .. } => {
            warn!(actor_id = %*target, peer_addr = %addr, "dead letter, frame exceeds the maximum frame size");
        }

        _ => warn!(peer_addr = %addr, "dropping a frame exceeding the maximum frame size"),
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ActorId,
        quota::Quota,
        remote::{
            frame::{Frame, Handshake, HandshakeError},
            node::NodeId,
            peer::{ConnectError, WriterEnd, exchange_handshakes, recv_handshake, write_frames},
            transport::{FrameReceiver, FrameSender, TransportError},
        },
    };
    use std::{borrow::Cow, collections::VecDeque, sync::mpsc, time::Duration};

    struct FakeReceiver {
        frames: VecDeque<Vec<u8>>,
        current: Vec<u8>,
    }

    impl FakeReceiver {
        fn new(frames: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                frames: frames.into_iter().collect(),
                current: Vec::new(),
            }
        }
    }

    impl FrameReceiver for FakeReceiver {
        async fn recv(&mut self) -> Result<Option<&[u8]>, TransportError> {
            match self.frames.pop_front() {
                Some(frame) => {
                    self.current = frame;
                    Ok(Some(&self.current))
                }

                None => Ok(None),
            }
        }
    }

    /// Receives nothing, ever: the wire shape of a peer which connects and stays silent.
    struct SilentReceiver;

    impl FrameReceiver for SilentReceiver {
        async fn recv(&mut self) -> Result<Option<&[u8]>, TransportError> {
            std::future::pending().await
        }
    }

    struct RecordingSender(mpsc::Sender<Vec<u8>>);

    impl FrameSender for RecordingSender {
        async fn send(&mut self, frame: &[u8]) -> Result<(), TransportError> {
            let _ = self.0.send(frame.to_vec());
            Ok(())
        }
    }

    fn peer() -> NodeId {
        NodeId::new("127.0.0.1:1234".parse().expect("valid address"))
    }

    /// A transport failure, a peer closing early or a peer staying silent is worth another
    /// attempt, while a peer which does not speak this protocol never becomes one, so retrying it
    /// forever is pure waste.
    #[test]
    fn only_transport_failures_are_retried() {
        assert!(ConnectError::Transport(TransportError::other("boom")).is_retryable());
        assert!(ConnectError::Closed.is_retryable());
        assert!(ConnectError::DataStreams(1).is_retryable());
        assert!(ConnectError::HandshakeTimeout.is_retryable());

        assert!(
            !ConnectError::Transport(TransportError::FrameTooLarge { len: 2, max: 1 })
                .is_retryable()
        );

        assert!(!ConnectError::NotAHandshake.is_retryable());
        assert!(!ConnectError::Decode(postcard::Error::DeserializeUnexpectedEnd).is_retryable());
        assert!(!ConnectError::Handshake(HandshakeError::Magic(0)).is_retryable());
        assert!(!ConnectError::Handshake(HandshakeError::ProtocolVersion(u16::MAX)).is_retryable());
    }

    /// The dial side's handshake wait is bounded like the accept side's: a peer which connects
    /// and never speaks times out into the retry path instead of pinning the lane's dial forever.
    #[tokio::test(start_paused = true)]
    async fn a_silent_peer_times_out_the_handshake() {
        let (sent_tx, _sent_rx) = mpsc::channel();
        let mut sender = RecordingSender(sent_tx);
        let mut receiver = SilentReceiver;

        let exchanged =
            exchange_handshakes(&mut sender, &mut receiver, peer(), Duration::from_secs(1)).await;

        assert!(matches!(exchanged, Err(ConnectError::HandshakeTimeout)));
    }

    /// A peer answering within the deadline is handshaked normally, so the bound costs the happy
    /// path nothing but the timer.
    #[tokio::test]
    async fn handshakes_exchange_within_the_deadline() {
        let remote = peer();
        let bytes = Frame::Handshake(Handshake::new(remote))
            .encode_into(Vec::new())
            .expect("handshake encodes");
        let (sent_tx, sent_rx) = mpsc::channel();
        let mut sender = RecordingSender(sent_tx);
        let mut receiver = FakeReceiver::new([bytes]);

        let exchanged =
            exchange_handshakes(&mut sender, &mut receiver, peer(), Duration::from_secs(1))
                .await
                .expect("handshakes are exchanged");

        assert_eq!(exchanged, remote);
        assert_eq!(sent_rx.try_iter().count(), 1);
    }

    /// An oversize frame slipping past the send-time check dies in the writer as the backstop:
    /// it never reaches the transport, whose receiver's refusal would kill the connection, and
    /// the frames behind it still ride the stream.
    #[tokio::test]
    async fn an_oversize_frame_dies_in_the_writer() {
        let (outbound_tx, outbound_rx) = flume::unbounded();
        outbound_tx
            .send(Frame::Message {
                target: ActorId::new(),
                reply_tags: Vec::new(),
                payload: Cow::Owned(vec![0; 64]),
            })
            .expect("the queue is open");
        outbound_tx.send(Frame::Ping).expect("the queue is open");
        drop(outbound_tx);

        let (sent_tx, sent_rx) = mpsc::channel();
        let addr = "127.0.0.1:1234".parse().expect("valid address");
        let end = write_frames(
            RecordingSender(sent_tx),
            outbound_rx,
            Quota::unbounded(),
            addr,
            32,
        )
        .await;

        assert!(matches!(end, WriterEnd::LaneClosed));
        let sent = sent_rx.try_iter().collect::<Vec<_>>();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0],
            Frame::Ping.encode_into(Vec::new()).expect("ping encodes")
        );
    }

    #[tokio::test]
    async fn handshake_is_accepted_from_a_waltz_node() {
        let peer = peer();
        let bytes = Frame::Handshake(Handshake::new(peer))
            .encode_into(Vec::new())
            .expect("handshake encodes");
        let mut receiver = FakeReceiver::new([bytes]);

        let handshaked = recv_handshake(&mut receiver)
            .await
            .expect("handshake is accepted");
        assert_eq!(handshaked, peer);
    }

    #[tokio::test]
    async fn a_closed_connection_is_reported_as_such() {
        let mut receiver = FakeReceiver::new([]);

        assert!(matches!(
            recv_handshake(&mut receiver).await,
            Err(ConnectError::Closed)
        ));
    }

    #[tokio::test]
    async fn a_first_frame_other_than_a_handshake_is_rejected() {
        let bytes = Frame::Ping.encode_into(Vec::new()).expect("ping encodes");
        let mut receiver = FakeReceiver::new([bytes]);

        assert!(matches!(
            recv_handshake(&mut receiver).await,
            Err(ConnectError::NotAHandshake)
        ));
    }

    #[tokio::test]
    async fn an_undecodable_first_frame_is_rejected() {
        let mut receiver = FakeReceiver::new([vec![0xff; 8]]);

        assert!(matches!(
            recv_handshake(&mut receiver).await,
            Err(ConnectError::Decode(_))
        ));
    }
}
