//! Remoting: actors on different nodes messaging each other through ordinary, serializable
//! [ActorRef]s (`remote` feature).
//!
//! Each process runs at most one remoting endpoint, started via [start] with a [Transport] (the
//! provided one is [QuicTransport]) and an [EndpointConfig]. [ActorRef] implements `Serialize`
//! and `Deserialize`, both failing with [RefError::EndpointNotStarted] until the endpoint is
//! started: message types simply embed reference fields, e.g. `reply_to: ActorRef<Reply>`, and
//! work unchanged no matter where their counterpart lives. Serializing a local reference lazily
//! binds the actor in the endpoint's registry, so inbound messages find it; the binding is evicted
//! once the actor terminates. Message payloads are encoded by a [Codec], by default [Postcard].
//!
//! Bootstrap is out of band: [serialize_ref] turns a local reference into bytes to be shared via
//! configuration, command line or any other channel, and [deserialize_ref] resolves them on
//! another node. Any further reference travels inside messages.
//!
//! # Guarantees
//!
//! Remote [ActorRef::tell] keeps the local contract: fire-and-forget, at-most-once, undeliverable
//! messages are dropped and logged as dead letters. This covers an unreachable or crashed node, a
//! full outbound queue and a payload which cannot be decoded on the receiving node. Messages from
//! one sender to one target arrive in send order: all frames towards a node ride one ordered
//! lane, enqueued at tell time and delivered to mailboxes in arrival order. A lost
//! connection is reconnected with backoff; frames queued while the link is down flush in order,
//! so per-sender FIFO across reconnects is "in order, with gaps".
//!
//! Death watch works across nodes through the ordinary [ActorContext::watch], with a two tier
//! contract: a terminated signal for a *real termination* keeps the full local guarantee (ordered
//! behind all messages the terminated actor delivered to the watcher and proving its destructors
//! have run), while a signal *synthesized* upon node death (decided by a [FailureDetector] fed
//! with heartbeats, or by a new incarnation appearing at a known address) only proves that no
//! message from that actor will ever be delivered through this endpoint again. The two are
//! indistinguishable in the API; see docs/remoting.md for the full contract and its rationale.
//!
//! [ActorContext::watch]: crate::ActorContext::watch
//! [ActorRef]: crate::ActorRef
//! [ActorRef::tell]: crate::ActorRef::tell

mod codec;
mod endpoint;
mod failure;
mod frame;
mod node;
mod peer;
mod quic;
mod registry;
mod sink;
mod transport;
mod watch;
mod wire;

pub use crate::remote::{
    codec::{Codec, CodecError, DecodeVisitor, Postcard},
    endpoint::{EndpointConfig, FailureDetectorFactory, StartError, start},
    failure::{DeadlineFailureDetector, FailureDetector},
    quic::{QuicConnection, QuicFrameReceiver, QuicFrameSender, QuicTransport},
    transport::{Connection, FrameReceiver, FrameSender, Transport, TransportError},
    wire::{RefError, deserialize_ref, serialize_ref},
};

pub(crate) use crate::remote::{
    node::NodeId,
    sink::RemoteSink,
    watch::{unwatch_remote, watch_remote},
};
