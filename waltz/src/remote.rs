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
//! Bootstrap goes through discovery: [register] names a local actor under a [Key] and [lookup]
//! resolves that name at a known address, so nothing but a name and an address has to be
//! configured. Alternatively [serialize_ref] turns a local reference into bytes to be shared via
//! configuration, command line or any other channel, and [deserialize_ref] resolves them on
//! another node, refusing bytes serialized for another message type as [RefError::TypeMismatch].
//! Any further reference travels inside messages.
//!
//! # Guarantees
//!
//! Remote [ActorRef::tell] keeps the local contract: fire-and-forget, at-most-once, undeliverable
//! messages are dropped and logged as dead letters. This covers an unreachable or crashed node, a
//! full outbound queue and a payload which cannot be decoded on the receiving node. Messages from
//! one sender to one target arrive in send order: all frames towards one actor ride one ordered
//! stream of the lane towards its node, enqueued at tell time and delivered to mailboxes in
//! arrival order, so a large message only delays frames towards actors sharing its stream. A lost
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
//! Request-response crosses nodes the same way: [ReplyTo] is serializable and travels inside
//! messages, so [ActorRef::ask] and [ActorContext::reply_to] work unchanged against remote
//! actors. An ask still resolves exactly once, at latest at its timeout, and a reply keeps
//! per-sender FIFO with the responder's other messages to the asker. The `NoReply` detection
//! weakens to best-effort: a reply destination dropped on another node is signalled
//! fire-and-forget, a request dead-lettered undecoded on the receiving node is answered the same
//! way, and node death, like a given-up lane, fails the asks pending towards that node; see
//! docs/remoting.md for the exact contract.
//!
//! [ActorContext::reply_to]: crate::ActorContext::reply_to
//! [ActorContext::watch]: crate::ActorContext::watch
//! [ActorRef]: crate::ActorRef
//! [ActorRef::ask]: crate::ActorRef::ask
//! [ActorRef::tell]: crate::ActorRef::tell
//! [ReplyTo]: crate::ReplyTo

mod codec;
mod discovery;
mod endpoint;
mod failure;
mod frame;
mod node;
mod peer;
mod quic;
mod registry;
mod reply;
mod sink;
mod transport;
mod watch;
mod wire;

pub use crate::remote::{
    codec::{Codec, CodecError, DecodeVisitor, Postcard},
    discovery::{Key, LookupError, RegisterError, lookup, register},
    endpoint::{EndpointConfig, FailureDetectorFactory, StartError, start},
    failure::{DeadlineFailureDetector, FailureDetector},
    quic::{QuicConnection, QuicFrameReceiver, QuicFrameSender, QuicTransport, QuicTransportError},
    transport::{
        ConnectedControl, Connection, FrameReceiver, FrameSender, Transport, TransportError,
    },
    wire::{RefError, deserialize_ref, serialize_ref},
};

pub(crate) use crate::remote::{
    node::NodeId,
    sink::RemoteSink,
    watch::{unwatch_remote, watch_remote},
};

/// Severs every live connection of this process's endpoint, for tests: frames in flight are
/// lost, lanes keep their queues and reconnect, which is the fault the reconnect guarantees are
/// stated for. `false` if the endpoint is not started.
///
/// Only available with the `remote-dev` feature, so it cannot reach a production build which
/// does not ask for it.
#[cfg(feature = "remote-dev")]
#[cfg_attr(docsrs, doc(cfg(feature = "remote-dev")))]
pub fn sever_connections() -> bool {
    match endpoint::get() {
        Some(endpoint) => {
            endpoint.sever();
            true
        }

        None => false,
    }
}

/// Arms the endpoint to silently drop the next `count` outbound terminated signal frames, for
/// tests: the loss the periodic watch refresh must heal. `false` if the endpoint is not started.
///
/// Only available with the `remote-dev` feature, so it cannot reach a production build which
/// does not ask for it.
#[cfg(feature = "remote-dev")]
#[cfg_attr(docsrs, doc(cfg(feature = "remote-dev")))]
pub fn drop_terminated_frames(count: u64) -> bool {
    match endpoint::get() {
        Some(endpoint) => {
            endpoint.arm_terminated_drop(count);
            true
        }

        None => false,
    }
}
