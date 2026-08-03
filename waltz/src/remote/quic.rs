use crate::remote::transport::{Connection, FrameReceiver, FrameSender, Transport, TransportError};
#[cfg(feature = "remote-dev")]
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{ClientConfig, Connecting, Endpoint, Incoming, RecvStream, SendStream, ServerConfig};
#[cfg(feature = "remote-dev")]
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime},
};
#[cfg(feature = "remote-dev")]
use std::sync::Arc;
use std::{io, net::SocketAddr};

#[cfg(feature = "remote-dev")]
const DEV_SERVER_NAME: &str = "waltz";

/// A QUIC [Transport] backed by quinn. TLS is mandatory for QUIC; use [QuicTransport::new] with
/// proper certificates for production and `QuicTransport::dev`, added by the `remote-dev`
/// feature, for development and tests.
#[derive(Debug)]
pub struct QuicTransport {
    endpoint: Endpoint,
    server_name: String,
}

impl QuicTransport {
    /// A transport bound to the given address with the given TLS configurations, validating the
    /// certificates of the nodes it connects to against `server_name`, which hence must be the
    /// name those certificates are issued for.
    pub fn new(
        bind_addr: SocketAddr,
        server_config: ServerConfig,
        client_config: ClientConfig,
        server_name: impl Into<String>,
    ) -> io::Result<Self> {
        let mut endpoint = Endpoint::server(server_config, bind_addr)?;
        endpoint.set_default_client_config(client_config);
        Ok(Self {
            endpoint,
            server_name: server_name.into(),
        })
    }

    /// A transport for development and tests only: a self signed certificate on the server side
    /// and no certificate verification on the client side. Never use this on untrusted networks!
    ///
    /// Only available with the `remote-dev` feature, so it cannot reach a production build
    /// which does not ask for it.
    #[cfg(feature = "remote-dev")]
    #[cfg_attr(docsrs, doc(cfg(feature = "remote-dev")))]
    pub fn dev(bind_addr: SocketAddr) -> io::Result<Self> {
        let certified_key = rcgen::generate_simple_self_signed(vec![DEV_SERVER_NAME.to_string()])
            .map_err(io::Error::other)?;
        let cert = certified_key.cert.der().clone();
        let key = PrivateKeyDer::Pkcs8(certified_key.key_pair.serialize_der().into());
        let server_config =
            ServerConfig::with_single_cert(vec![cert], key).map_err(io::Error::other)?;

        let tls_config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert(
                rustls::crypto::ring::default_provider(),
            )))
            .with_no_client_auth();
        let tls_config = QuicClientConfig::try_from(tls_config).map_err(io::Error::other)?;
        let client_config = ClientConfig::new(Arc::new(tls_config));

        Self::new(bind_addr, server_config, client_config, DEV_SERVER_NAME)
    }

    /// The actually bound local address, e.g. for advertising a port chosen by the OS.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }
}

impl Transport for QuicTransport {
    type Connection = QuicConnection;

    async fn connect(&self, addr: SocketAddr) -> Result<QuicConnection, TransportError> {
        let connecting = self
            .endpoint
            .connect(addr, &self.server_name)
            .map_err(TransportError::other)?;
        Ok(QuicConnection(QuicConnectionInner::Dialed(connecting)))
    }

    async fn accept(&self) -> Result<QuicConnection, TransportError> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| TransportError::other("QUIC endpoint closed"))?;
        Ok(QuicConnection(QuicConnectionInner::Accepted(Box::new(
            incoming,
        ))))
    }
}

/// A connection produced by [QuicTransport]: one bidirectional QUIC stream carrying length
/// delimited frames.
#[derive(Debug)]
pub struct QuicConnection(QuicConnectionInner);

impl Connection for QuicConnection {
    type Sender = QuicFrameSender;
    type Receiver = QuicFrameReceiver;

    async fn open(
        self,
        max_frame_size: usize,
    ) -> Result<(QuicFrameSender, QuicFrameReceiver), TransportError> {
        let (stream_sender, stream_receiver, connection) = match self.0 {
            QuicConnectionInner::Dialed(connecting) => {
                let connection = connecting.await.map_err(TransportError::other)?;
                let (sender, receiver) =
                    connection.open_bi().await.map_err(TransportError::other)?;
                (sender, receiver, connection)
            }

            QuicConnectionInner::Accepted(incoming) => {
                let connection = (*incoming).await.map_err(TransportError::other)?;
                let (sender, receiver) = connection
                    .accept_bi()
                    .await
                    .map_err(TransportError::other)?;
                (sender, receiver, connection)
            }
        };

        let sender = QuicFrameSender {
            stream: stream_sender,
            _connection: connection.clone(),
        };
        let receiver = QuicFrameReceiver {
            stream: stream_receiver,
            max_frame_size,
            _connection: connection,
        };
        Ok((sender, receiver))
    }
}

/// The sending half of a [QuicConnection].
#[derive(Debug)]
pub struct QuicFrameSender {
    stream: SendStream,

    /// Keeps the connection alive: quinn closes a connection once its last handle is dropped.
    _connection: quinn::Connection,
}

impl FrameSender for QuicFrameSender {
    async fn send(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        let len = u32::try_from(frame.len()).map_err(TransportError::other)?;
        self.stream
            .write_all(&len.to_be_bytes())
            .await
            .map_err(TransportError::other)?;
        self.stream
            .write_all(frame)
            .await
            .map_err(TransportError::other)
    }
}

/// The receiving half of a [QuicConnection].
#[derive(Debug)]
pub struct QuicFrameReceiver {
    stream: RecvStream,
    max_frame_size: usize,

    /// Keeps the connection alive: quinn closes a connection once its last handle is dropped, so
    /// both halves must hold one to stay usable independently.
    _connection: quinn::Connection,
}

impl FrameReceiver for QuicFrameReceiver {
    async fn recv(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        let mut len = [0; 4];
        match self.stream.read_exact(&mut len).await {
            Ok(()) => {}
            Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
            Err(error) => return Err(TransportError::other(error)),
        }

        let len = usize::try_from(u32::from_be_bytes(len)).map_err(TransportError::other)?;
        if len > self.max_frame_size {
            return Err(TransportError::other(format!(
                "frame of {len} bytes exceeds the maximum frame size"
            )));
        }

        let mut frame = vec![0; len];
        self.stream
            .read_exact(&mut frame)
            .await
            .map_err(TransportError::other)?;
        Ok(Some(frame))
    }
}

#[derive(Debug)]
enum QuicConnectionInner {
    Dialed(Connecting),
    Accepted(Box<Incoming>),
}

#[cfg(feature = "remote-dev")]
#[derive(Debug)]
struct AcceptAnyServerCert(CryptoProvider);

#[cfg(feature = "remote-dev")]
impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(all(test, feature = "remote-dev"))]
mod tests {
    use crate::remote::{
        quic::QuicTransport,
        transport::{Connection, FrameReceiver, FrameSender, Transport},
    };
    use std::{
        io,
        net::{Ipv4Addr, SocketAddr},
        time::Duration,
    };
    use tokio::time::timeout;

    const MAX_FRAME_SIZE: usize = 64;
    const TIMEOUT: Duration = Duration::from_secs(10);

    /// A connected pair of framing halves plus the dialer's own receiver, which the caller must
    /// keep alive: it holds the dialer's connection, and dropping the last handle would close the
    /// connection instead of finishing the stream.
    ///
    /// The halves only pair up once the dialer has written, since a QUIC stream is invisible to
    /// the acceptor until then.
    async fn connected(
        max_frame_size: usize,
        first: &[u8],
    ) -> io::Result<(impl FrameSender, impl FrameReceiver, impl FrameReceiver)> {
        let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let server = QuicTransport::dev(loopback)?;
        let addr = server.local_addr()?;
        let client = QuicTransport::dev(loopback)?;

        let accepting = tokio::spawn(async move {
            let connection = server.accept().await.expect("accepts");
            connection.open(max_frame_size).await.expect("opens")
        });

        let connection = client.connect(addr).await.expect("connects");
        let (mut sender, keep_alive) = connection.open(max_frame_size).await.expect("opens");
        sender.send(first).await.expect("sends the first frame");

        let (_sender, receiver) = accepting.await.expect("accept task");
        Ok((sender, keep_alive, receiver))
    }

    /// Frames survive the length delimited framing intact and in order.
    #[tokio::test]
    async fn frames_round_trip_in_order() {
        let (mut sender, _keep_alive, mut receiver) =
            timeout(TIMEOUT, connected(MAX_FRAME_SIZE, b"first"))
                .await
                .expect("connects in time")
                .expect("connects");

        sender.send(b"second").await.expect("sends");

        assert_eq!(
            timeout(TIMEOUT, receiver.recv())
                .await
                .expect("in time")
                .expect("receives"),
            Some(b"first".to_vec())
        );
        assert_eq!(
            timeout(TIMEOUT, receiver.recv())
                .await
                .expect("in time")
                .expect("receives"),
            Some(b"second".to_vec())
        );
    }

    /// A peer closing the stream ends the frames rather than failing: the receiver reports the
    /// end of stream once the sender is gone.
    #[tokio::test]
    async fn a_closed_stream_ends_the_frames() {
        let (sender, _keep_alive, mut receiver) =
            timeout(TIMEOUT, connected(MAX_FRAME_SIZE, b"only"))
                .await
                .expect("connects in time")
                .expect("connects");

        assert_eq!(
            timeout(TIMEOUT, receiver.recv())
                .await
                .expect("in time")
                .expect("receives"),
            Some(b"only".to_vec())
        );

        drop(sender);

        assert_eq!(
            timeout(TIMEOUT, receiver.recv())
                .await
                .expect("in time")
                .expect("receives"),
            None
        );
    }

    /// A frame beyond the connection's maximum is refused instead of allocating for it, which is
    /// what keeps a peer from naming an arbitrary length.
    #[tokio::test]
    async fn an_oversize_frame_is_refused() {
        let (mut sender, _keep_alive, mut receiver) =
            timeout(TIMEOUT, connected(MAX_FRAME_SIZE, b"small"))
                .await
                .expect("connects in time")
                .expect("connects");

        assert_eq!(
            timeout(TIMEOUT, receiver.recv())
                .await
                .expect("in time")
                .expect("receives"),
            Some(b"small".to_vec())
        );

        sender.send(&[0; MAX_FRAME_SIZE + 1]).await.expect("sends");

        assert!(
            timeout(TIMEOUT, receiver.recv())
                .await
                .expect("in time")
                .is_err()
        );
    }
}
