use crate::{ActorId, remote::node::NodeId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const PROTOCOL_MAGIC: u32 = 0x574C_545A;
const PROTOCOL_VERSION: u16 = 1;

/// System frames ride the same FIFO lane as messages, which is what carries the watch ordering
/// guarantee across the wire, and bypass the outbound capacity, since a terminated signal must
/// never be dropped.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Frame {
    Handshake(Handshake),

    Message { target: ActorId, payload: Vec<u8> },

    Watch { target: ActorId },

    Unwatch { target: ActorId },

    Terminated { target: ActorId },

    Ping,
}

impl Frame {
    pub(crate) fn encode_into(&self, mut buffer: Vec<u8>) -> Result<Vec<u8>, postcard::Error> {
        buffer.clear();
        postcard::to_extend(self, buffer)
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }

    /// Whether the frame is subject to the outbound capacity; matched exhaustively, so a new
    /// variant has to decide instead of silently bypassing it.
    pub(crate) fn is_message(&self) -> bool {
        match self {
            Frame::Message { .. } => true,

            Frame::Handshake(_)
            | Frame::Watch { .. }
            | Frame::Unwatch { .. }
            | Frame::Terminated { .. }
            | Frame::Ping => false,
        }
    }
}

/// Deliberately neither [Clone] nor [Copy]: [Handshake::validate] consumes it, so only the
/// validated [NodeId] survives.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Handshake {
    magic: u32,
    protocol_version: u16,
    node: NodeId,
}

impl Handshake {
    pub(crate) fn new(node: NodeId) -> Self {
        Self {
            magic: PROTOCOL_MAGIC,
            protocol_version: PROTOCOL_VERSION,
            node,
        }
    }

    pub(crate) fn validate(self) -> Result<NodeId, HandshakeError> {
        if self.magic != PROTOCOL_MAGIC {
            return Err(HandshakeError::Magic(self.magic));
        }
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(HandshakeError::ProtocolVersion(self.protocol_version));
        }
        Ok(self.node)
    }
}

#[derive(Debug, Error)]
pub(crate) enum HandshakeError {
    #[error("unexpected protocol magic {0:#010x}")]
    Magic(u32),

    #[error("unsupported protocol version {0}")]
    ProtocolVersion(u16),
}

#[cfg(test)]
mod tests {
    use crate::{
        ActorId,
        remote::{
            frame::{Frame, Handshake, HandshakeError, PROTOCOL_MAGIC, PROTOCOL_VERSION},
            node::NodeId,
        },
    };

    /// Every frame survives a round trip through the wire format; a change to the variant order
    /// or to a field breaks this, which is what makes the format explicit rather than incidental.
    #[test]
    fn frames_round_trip() {
        let node = NodeId::new("127.0.0.1:1234".parse().expect("valid address"));
        let target = ActorId::new();

        let frames = [
            Frame::Handshake(Handshake::new(node)),
            Frame::Message {
                target,
                payload: vec![1, 2, 3],
            },
            Frame::Watch { target },
            Frame::Unwatch { target },
            Frame::Terminated { target },
            Frame::Ping,
        ];

        for frame in frames {
            let bytes = frame.encode_into(Vec::new()).expect("frame encodes");
            let decoded = Frame::from_bytes(&bytes).expect("frame decodes");
            assert_eq!(format!("{frame:?}"), format!("{decoded:?}"));
        }
    }

    /// Only message frames are subject to the outbound capacity; system frames bypass it, since a
    /// terminated signal must never be dropped.
    #[test]
    fn only_messages_are_subject_to_capacity() {
        let target = ActorId::new();

        assert!(
            Frame::Message {
                target,
                payload: vec![]
            }
            .is_message()
        );
        assert!(!Frame::Watch { target }.is_message());
        assert!(!Frame::Unwatch { target }.is_message());
        assert!(!Frame::Terminated { target }.is_message());
        assert!(!Frame::Ping.is_message());
    }

    #[test]
    fn handshake_accepts_this_protocol() {
        let node = NodeId::new("127.0.0.1:1234".parse().expect("valid address"));

        let peer = Handshake::new(node).validate().expect("handshake is valid");
        assert_eq!(peer, node);
    }

    #[test]
    fn handshake_rejects_alien_magic() {
        let node = NodeId::new("127.0.0.1:1234".parse().expect("valid address"));
        let handshake = Handshake {
            magic: !PROTOCOL_MAGIC,
            protocol_version: PROTOCOL_VERSION,
            node,
        };

        assert!(matches!(
            handshake.validate(),
            Err(HandshakeError::Magic(_))
        ));
    }

    #[test]
    fn handshake_rejects_other_protocol_versions() {
        let node = NodeId::new("127.0.0.1:1234".parse().expect("valid address"));
        let handshake = Handshake {
            magic: PROTOCOL_MAGIC,
            protocol_version: PROTOCOL_VERSION + 1,
            node,
        };

        assert!(matches!(
            handshake.validate(),
            Err(HandshakeError::ProtocolVersion(_))
        ));
    }
}
