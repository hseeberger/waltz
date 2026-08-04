use std::{error::Error, io, net::SocketAddr};
use thiserror::Error as ThisError;

/// A driver establishing connections between remoting endpoints; the provided one is
/// [QuicTransport](crate::remote::QuicTransport).
#[trait_variant::make(Send)]
pub trait Transport
where
    Self: Send + Sync + 'static,
{
    /// The type of the connections established by this transport.
    type Connection: Connection;

    /// Connect to the node listening at the given address.
    async fn connect(&self, addr: SocketAddr) -> Result<Self::Connection, TransportError>;

    /// Wait for the next inbound connection.
    async fn accept(&self) -> Result<Self::Connection, TransportError>;
}

/// A connection produced by a [Transport], not yet carrying frames.
#[trait_variant::make(Send)]
pub trait Connection
where
    Self: Send + 'static,
{
    /// The sending half of the open connection.
    type Sender: FrameSender;

    /// The receiving half of the open connection.
    type Receiver: FrameReceiver;

    /// Open the frame lane, refusing inbound frames larger than `max_frame_size` bytes. Frames
    /// sent on the returned sender must be received by the peer in send order or the connection
    /// must die; a transport must never reorder them.
    async fn open(
        self,
        max_frame_size: usize,
    ) -> Result<(Self::Sender, Self::Receiver), TransportError>;
}

/// The sending half of an open [Connection].
#[trait_variant::make(Send)]
pub trait FrameSender
where
    Self: Send + 'static,
{
    /// Send one frame; the bytes are only read, never retained.
    async fn send(&mut self, frame: &[u8]) -> Result<(), TransportError>;
}

/// The receiving half of an open [Connection].
#[trait_variant::make(Send)]
pub trait FrameReceiver
where
    Self: Send + 'static,
{
    /// Receive the next frame; `None` once the peer has closed the connection.
    async fn recv(&mut self) -> Result<Option<Vec<u8>>, TransportError>;
}

/// A connection or frame which a [Transport] cannot carry, wrapping the driver's failure
/// whatever its kind: I/O, TLS, protocol or otherwise.
#[derive(Debug, ThisError)]
#[error(transparent)]
pub struct TransportError(Box<dyn Error + Send + Sync>);

impl TransportError {
    /// Wrap any error as a transport failure.
    pub fn other<E>(error: E) -> Self
    where
        E: Into<Box<dyn Error + Send + Sync>>,
    {
        Self(error.into())
    }
}

impl From<io::Error> for TransportError {
    fn from(error: io::Error) -> Self {
        Self::other(error)
    }
}
