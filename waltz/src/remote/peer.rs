use crate::{
    quota::Quota,
    remote::{
        endpoint::{EndpointInner, Generation},
        failure::DeliveryGate,
        frame::{Frame, Handshake, HandshakeError},
        node::NodeId,
        transport::{Connection, FrameReceiver, FrameSender, Transport, TransportError},
        watch,
    },
};
use flume::Receiver;
use log::{debug, error, warn};
use std::{net::SocketAddr, sync::Arc};
use thiserror::Error;
use tokio::{
    task::{self, JoinHandle},
    time::sleep,
};

pub(crate) struct DialRequest {
    pub(crate) addr: SocketAddr,
    pub(crate) lane_id: Generation,
    pub(crate) outbound_rx: Receiver<Frame>,
    pub(crate) quota: Quota,
}

/// A transport failure is transient and worth another attempt, everything else says the peer
/// does not speak this protocol and never will.
#[derive(Debug, Error)]
pub(crate) enum ConnectError {
    #[error(transparent)]
    Transport(#[from] TransportError),

    #[error("connection closed before handshake")]
    Closed,

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
            ConnectError::Transport(_) | ConnectError::Closed => true,

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
        match transport.accept().await {
            Ok(connection) => {
                task::spawn(run_accepted(connection, endpoint));
            }

            Err(error) => {
                error!(error:%; "remoting endpoint cannot accept connections");
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

async fn run_accepted<C>(connection: C, endpoint: &'static EndpointInner)
where
    C: Connection,
{
    let (mut frame_sender, mut frame_receiver) = match connection
        .open(endpoint.config().max_frame_size.get())
        .await
    {
        Ok(halves) => halves,
        Err(error) => {
            warn!(error:%; "cannot open inbound connection");
            return;
        }
    };

    let peer = match recv_handshake(&mut frame_receiver).await {
        Ok(peer) => peer,
        Err(error) => {
            warn!(error:%; "cannot receive inbound handshake");
            return;
        }
    };
    if let Err(error) = send_handshake(&mut frame_sender, endpoint).await {
        warn!(peer:% = peer, error:%; "cannot send outbound handshake");
        return;
    }

    let Some(reader) = admit(peer, frame_receiver, endpoint) else {
        return;
    };
    debug!(peer:% = peer; "inbound connection established");

    let reader_id = endpoint.supersede_inbound_reader(peer.addr(), reader.abort_handle());
    let _ = reader.await;
    endpoint.remove_inbound_reader(peer.addr(), reader_id);

    drop(frame_sender);
}

/// A connection's reader must be aborted before the next one is dialed: two readers for one peer
/// could deliver a frame buffered on the dead connection behind one from the new connection.
async fn run_peer<T>(transport: Arc<T>, request: DialRequest, endpoint: &'static EndpointInner)
where
    T: Transport,
{
    let DialRequest {
        addr,
        lane_id,
        outbound_rx,
        quota,
    } = request;
    let mut attempts = 0u32;

    loop {
        match connect(transport.as_ref(), addr, endpoint).await {
            Ok((frame_sender, frame_receiver, peer)) => {
                let Some(reader) = admit(peer, frame_receiver, endpoint) else {
                    break;
                };
                endpoint.bind_lane(addr, lane_id, peer);
                attempts = 0;
                debug!(peer:% = peer; "outbound connection established");

                // Watch frames lost with a connection must be re-sent here; idempotent remotely.
                for target in endpoint.watchers().targets(peer) {
                    if let Err(error) = endpoint.send(peer, Frame::Watch { target }) {
                        warn!(peer:% = peer, actor_id:% = target, error:%; "cannot re-establish remote watch");
                    }
                }

                let end = write_frames(frame_sender, &outbound_rx, &quota, addr).await;
                reader.abort();

                if matches!(end, WriterEnd::LaneClosed) {
                    break;
                }
            }

            Err(error) if !error.is_retryable() => {
                warn!(peer_addr:% = addr, error:%; "giving up connecting to node, not a waltz node of this protocol version");
                endpoint.refuse(addr);
                break;
            }

            Err(error) => {
                attempts += 1;
                debug!(peer_addr:% = addr, attempts, error:%; "cannot connect to node");

                if !endpoint.has_watches_involving(addr)
                    && attempts >= endpoint.config().max_connect_attempts
                {
                    warn!(peer_addr:% = addr; "giving up connecting to node");
                    break;
                }

                sleep(endpoint.reconnect_backoff(attempts)).await;
            }
        }

        if !endpoint.is_lane_open(addr, lane_id) {
            break;
        }
    }

    endpoint.remove_lane(addr, lane_id);
    drain_dead_letters(outbound_rx, addr).await;
}

async fn connect<T>(
    transport: &T,
    addr: SocketAddr,
    endpoint: &EndpointInner,
) -> Result<
    (
        <T::Connection as Connection>::Sender,
        <T::Connection as Connection>::Receiver,
        NodeId,
    ),
    ConnectError,
>
where
    T: Transport,
{
    let connection = transport.connect(addr).await?;
    let (mut frame_sender, mut frame_receiver) = connection
        .open(endpoint.config().max_frame_size.get())
        .await?;

    send_handshake(&mut frame_sender, endpoint).await?;
    let peer = recv_handshake(&mut frame_receiver).await?;

    Ok((frame_sender, frame_receiver, peer))
}

enum WriterEnd {
    LaneClosed,
    ConnectionLost,
}

enum WriteError {
    Encode(postcard::Error),
    Send(TransportError),
}

async fn write_frames<S>(
    mut frame_sender: S,
    outbound_rx: &Receiver<Frame>,
    quota: &Quota,
    addr: SocketAddr,
) -> WriterEnd
where
    S: FrameSender,
{
    let mut buffer = Vec::new();

    loop {
        let Ok(frame) = outbound_rx.recv_async().await else {
            return WriterEnd::LaneClosed;
        };
        let is_message = frame.is_message();

        let outcome = match frame.encode_into(buffer) {
            Ok(bytes) => {
                let outcome = frame_sender.send(&bytes).await.map_err(WriteError::Send);
                buffer = bytes;
                outcome
            }

            Err(error) => {
                buffer = Vec::new();
                Err(WriteError::Encode(error))
            }
        };

        if is_message {
            quota.unreserve();
        }

        match outcome {
            Ok(()) => {}

            Err(WriteError::Encode(error)) => {
                warn!(peer_addr:% = addr, error:%; "cannot encode frame");
            }

            Err(WriteError::Send(error)) => {
                warn!(peer_addr:% = addr, error:%; "connection lost");
                dead_letter(frame, addr);
                return WriterEnd::ConnectionLost;
            }
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
) where
    R: FrameReceiver,
{
    loop {
        let bytes = match frame_receiver.recv().await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                debug!(peer:% = peer; "connection closed by peer");
                return;
            }
            Err(error) => {
                debug!(peer:% = peer, error:%; "connection lost");
                return;
            }
        };

        let frame = match Frame::from_bytes(&bytes) {
            Ok(frame) => frame,
            Err(error) => {
                warn!(peer:% = peer, error:%; "closing connection, cannot decode frame");
                return;
            }
        };

        endpoint.record_heartbeat(peer.incarnation());

        let _guard = gate.enter();
        if endpoint.tombstoned(peer.incarnation()) {
            debug!(peer:% = peer; "closing connection to a dead node incarnation");
            return;
        }

        match frame {
            Frame::Message { target, payload } => {
                if let Err(error) = endpoint
                    .registry()
                    .deliver(target, &payload, endpoint.codec())
                {
                    warn!(peer:% = peer, actor_id:% = target, error:%; "dead letter");
                }
            }

            Frame::Watch { target } => watch::on_watch(endpoint, peer, target),

            Frame::Unwatch { target } => watch::on_unwatch(endpoint, peer, target),

            Frame::Terminated { target } => watch::on_terminated(endpoint, peer, target),

            Frame::Ping => {}

            Frame::Handshake(_) => {
                warn!(peer:% = peer; "closing connection, unexpected handshake frame");
                return;
            }
        }
    }
}

/// The tombstone check must come before [EndpointInner::note_peer]: refusing a dead
/// incarnation's handshake is what backs the synthesized signals.
fn admit<R>(
    peer: NodeId,
    frame_receiver: R,
    endpoint: &'static EndpointInner,
) -> Option<JoinHandle<()>>
where
    R: FrameReceiver,
{
    if endpoint.tombstoned(peer.incarnation()) {
        warn!(peer:% = peer; "refusing connection, dead node incarnation");
        return None;
    }

    let gate = endpoint.note_peer(peer);
    Some(task::spawn(read_frames(
        frame_receiver,
        peer,
        gate,
        endpoint,
    )))
}

async fn send_handshake<S>(
    frame_sender: &mut S,
    endpoint: &EndpointInner,
) -> Result<(), TransportError>
where
    S: FrameSender,
{
    let bytes = Frame::Handshake(Handshake::new(endpoint.node()))
        .encode_into(Vec::new())
        .map_err(TransportError::other)?;
    frame_sender.send(&bytes).await
}

async fn recv_handshake<R>(frame_receiver: &mut R) -> Result<NodeId, ConnectError>
where
    R: FrameReceiver,
{
    let bytes = frame_receiver.recv().await?.ok_or(ConnectError::Closed)?;

    match Frame::from_bytes(&bytes)? {
        Frame::Handshake(handshake) => Ok(handshake.validate()?),
        _ => Err(ConnectError::NotAHandshake),
    }
}

async fn drain_dead_letters(outbound_rx: Receiver<Frame>, addr: SocketAddr) {
    while let Ok(frame) = outbound_rx.recv_async().await {
        dead_letter(frame, addr);
    }
}

fn dead_letter(frame: Frame, addr: SocketAddr) {
    if let Frame::Message { target, .. } = frame {
        warn!(actor_id:% = target, peer_addr:% = addr; "dead letter, node unreachable");
    }
}

#[cfg(test)]
mod tests {
    use crate::remote::{
        frame::{Frame, Handshake, HandshakeError},
        node::NodeId,
        peer::{ConnectError, recv_handshake},
        transport::{FrameReceiver, TransportError},
    };
    use std::collections::VecDeque;

    struct FakeReceiver(VecDeque<Vec<u8>>);

    impl FakeReceiver {
        fn new(frames: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self(frames.into_iter().collect())
        }
    }

    impl FrameReceiver for FakeReceiver {
        async fn recv(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
            Ok(self.0.pop_front())
        }
    }

    fn peer() -> NodeId {
        NodeId::new("127.0.0.1:1234".parse().expect("valid address"))
    }

    /// A transport failure or a peer closing early is worth another attempt, while a peer which
    /// does not speak this protocol never becomes one, so retrying it forever is pure waste.
    #[test]
    fn only_transport_failures_are_retried() {
        assert!(ConnectError::Transport(TransportError::other("boom")).is_retryable());
        assert!(ConnectError::Closed.is_retryable());

        assert!(!ConnectError::NotAHandshake.is_retryable());
        assert!(!ConnectError::Handshake(HandshakeError::Magic(0)).is_retryable());
        assert!(!ConnectError::Handshake(HandshakeError::ProtocolVersion(u16::MAX)).is_retryable());
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
