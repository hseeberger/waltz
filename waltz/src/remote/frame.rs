use crate::{
    ActorId,
    remote::{
        discovery::{LookupResult, Nonce, WireKey},
        node::NodeId,
        reply::ReplyTag,
    },
};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use thiserror::Error;

const PROTOCOL_MAGIC: u32 = 0x574C_545A;
const PROTOCOL_VERSION: u16 = 1;

/// System frames bypass the outbound capacity, since a terminated signal must never be dropped;
/// [Frame::Reply] carries a user payload and counts like a message. [Frame::Message],
/// [Frame::Terminated], [Frame::Reply] and [Frame::ReplyDropped] ride the data stream their
/// recipient hashes onto, every other frame the control stream; naming the watcher is what puts a
/// terminated signal on the same stream as the messages the terminated actor sent it, which is
/// what carries the watch ordering guarantee across the wire, and naming the asker is what does
/// the same for a reply.
///
/// The payload is a [Cow]: queued outbound frames own it (`Frame<'static>`), while
/// [Frame::from_bytes] borrows it straight from the receive buffer, so an inbound payload is
/// never copied on its way into [deliver](crate::remote::registry::Registry::deliver).
///
/// A message repeats its payload's reply destinations as [ReplyTag]s next to the payload, so a
/// node which dead-letters it undecoded can still answer each with [Frame::ReplyDropped].
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Frame<'a> {
    Handshake(Handshake),

    Message {
        target: ActorId,
        reply_tags: Vec<ReplyTag>,
        #[serde(borrow)]
        payload: Cow<'a, [u8]>,
    },

    Watch {
        target: ActorId,
        watcher: ActorId,
    },

    Unwatch {
        target: ActorId,
        watcher: ActorId,
    },

    Terminated {
        target: ActorId,
        watcher: ActorId,
    },

    Lookup {
        nonce: Nonce,
        key: WireKey,
    },

    LookupReply {
        nonce: Nonce,
        result: LookupResult,
    },

    Reply {
        nonce: Nonce,
        recipient: ActorId,
        #[serde(borrow)]
        payload: Cow<'a, [u8]>,
    },

    ReplyDropped {
        nonce: Nonce,
        recipient: ActorId,
    },

    Ping,
}

impl<'a> Frame<'a> {
    pub(crate) fn encode_into(&self, mut buffer: Vec<u8>) -> Result<Vec<u8>, postcard::Error> {
        buffer.clear();
        postcard::to_extend(self, buffer)
    }

    pub(crate) fn from_bytes(bytes: &'a [u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }

    /// Whether the frame is subject to the outbound capacity; matched exhaustively, so a new
    /// variant has to decide instead of silently bypassing it.
    pub(crate) fn is_message(&self) -> bool {
        match self {
            Frame::Message { .. } | Frame::Reply { .. } => true,

            Frame::Handshake(_)
            | Frame::Watch { .. }
            | Frame::Unwatch { .. }
            | Frame::Terminated { .. }
            | Frame::Lookup { .. }
            | Frame::LookupReply { .. }
            | Frame::ReplyDropped { .. }
            | Frame::Ping => false,
        }
    }

    /// The actor on the peer this frame is delivered to, which picks its data stream; [None] rides
    /// the control stream. A terminated signal names the watcher rather than the terminated actor,
    /// so it shares a stream with the messages that actor sent the watcher, which is what keeps it
    /// ordered behind them; a reply names the asker for the same reason.
    pub(crate) fn recipient(&self) -> Option<ActorId> {
        match self {
            Frame::Message { target, .. } => Some(*target),

            Frame::Terminated { watcher, .. } => Some(*watcher),

            Frame::Reply { recipient, .. } | Frame::ReplyDropped { recipient, .. } => {
                Some(*recipient)
            }

            Frame::Handshake(_)
            | Frame::Watch { .. }
            | Frame::Unwatch { .. }
            | Frame::Lookup { .. }
            | Frame::LookupReply { .. }
            | Frame::Ping => None,
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
            discovery::{LookupResult, Nonce, WireKey},
            frame::{Frame, Handshake, HandshakeError, PROTOCOL_MAGIC, PROTOCOL_VERSION},
            node::NodeId,
            reply::ReplyTag,
        },
    };
    use std::borrow::Cow;

    /// A terminated signal rides the watcher's stream: routing it by target would break its
    /// ordering behind the messages the terminated actor sent to that watcher.
    #[test]
    fn terminated_rides_the_watchers_stream() {
        let target = ActorId::new();
        let watcher = ActorId::new();

        let message = Frame::Message {
            target,
            reply_tags: Vec::new(),
            payload: Cow::Borrowed(&[]),
        };
        assert_eq!(message.recipient(), Some(target));
        assert_eq!(
            Frame::Terminated { target, watcher }.recipient(),
            Some(watcher)
        );
        assert_eq!(
            Frame::Reply {
                nonce: Nonce::first(),
                recipient: watcher,
                payload: Cow::Borrowed(&[]),
            }
            .recipient(),
            Some(watcher)
        );
        assert_eq!(
            Frame::ReplyDropped {
                nonce: Nonce::first(),
                recipient: watcher,
            }
            .recipient(),
            Some(watcher)
        );
        assert_eq!(Frame::Watch { target, watcher }.recipient(), None);
        assert_eq!(Frame::Ping.recipient(), None);
    }

    /// Protocol version 1 is pinned to hardcoded wire bytes: a round trip cannot catch a format
    /// break, since reordered variants change both directions at once.
    #[test]
    fn ping_matches_its_pinned_wire_bytes() {
        let bytes = [9];

        let frame = Frame::from_bytes(&bytes).expect("frame decodes");
        assert!(matches!(frame, Frame::Ping));

        assert_eq!(
            Frame::Ping.encode_into(Vec::new()).expect("frame encodes"),
            bytes
        );
    }

    /// Every frame survives a round trip through the wire format; a change to the variant order
    /// or to a field breaks this, which is what makes the format explicit rather than incidental.
    #[test]
    fn frames_round_trip() {
        let node = NodeId::new("127.0.0.1:1234".parse().expect("valid address"));
        let target = ActorId::new();
        let watcher = ActorId::new();

        let frames = [
            Frame::Handshake(Handshake::new(node)),
            Frame::Message {
                target,
                reply_tags: vec![ReplyTag {
                    nonce: Nonce::first(),
                    recipient: watcher,
                }],
                payload: Cow::Borrowed(&[1, 2, 3]),
            },
            Frame::Watch { target, watcher },
            Frame::Unwatch { target, watcher },
            Frame::Terminated { target, watcher },
            Frame::Lookup {
                nonce: Nonce::first(),
                key: WireKey::new::<u64>("worker-pool"),
            },
            Frame::LookupReply {
                nonce: Nonce::first(),
                result: LookupResult::Found { node, id: target },
            },
            Frame::Reply {
                nonce: Nonce::first(),
                recipient: watcher,
                payload: Cow::Borrowed(&[4, 5, 6]),
            },
            Frame::ReplyDropped {
                nonce: Nonce::first(),
                recipient: watcher,
            },
            Frame::Ping,
        ];

        for frame in frames {
            let bytes = frame.encode_into(Vec::new()).expect("frame encodes");
            let decoded = Frame::from_bytes(&bytes).expect("frame decodes");
            assert_eq!(format!("{frame:?}"), format!("{decoded:?}"));
        }
    }

    /// Only message and reply frames are subject to the outbound capacity; system frames bypass
    /// it, since a terminated signal, like a reply-dropped notification, must never be dropped.
    #[test]
    fn only_messages_are_subject_to_capacity() {
        let target = ActorId::new();
        let watcher = ActorId::new();

        assert!(
            Frame::Message {
                target,
                reply_tags: Vec::new(),
                payload: Cow::Borrowed(&[])
            }
            .is_message()
        );
        assert!(
            Frame::Reply {
                nonce: Nonce::first(),
                recipient: watcher,
                payload: Cow::Borrowed(&[]),
            }
            .is_message()
        );
        assert!(
            !Frame::ReplyDropped {
                nonce: Nonce::first(),
                recipient: watcher,
            }
            .is_message()
        );
        assert!(!Frame::Watch { target, watcher }.is_message());
        assert!(!Frame::Unwatch { target, watcher }.is_message());
        assert!(!Frame::Terminated { target, watcher }.is_message());
        assert!(
            !Frame::Lookup {
                nonce: Nonce::first(),
                key: WireKey::new::<u64>("worker-pool"),
            }
            .is_message()
        );
        assert!(
            !Frame::LookupReply {
                nonce: Nonce::first(),
                result: LookupResult::NotFound,
            }
            .is_message()
        );
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
