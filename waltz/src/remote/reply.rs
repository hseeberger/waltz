use crate::{
    ActorId, ReplyTo,
    ask::SendReply,
    remote::{
        codec::{Codec, CodecError},
        discovery::Nonce,
        endpoint::{self, EndpointInner},
        frame::Frame,
        node::NodeId,
        sink::{RemoteSendError, admit_payload},
        wire::RefError,
    },
    sync::lock,
};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, DeserializeOwned},
    ser,
};
use std::{
    any::{Any, type_name},
    borrow::Cow,
    cell::RefCell,
    collections::HashMap,
    mem,
    net::SocketAddr,
    sync::{Mutex, atomic::AtomicU64},
};
use tracing::{debug, warn};

type DeliverReply = fn(Box<dyn Any + Send>, &[u8], &dyn Codec) -> Result<(), CodecError>;

thread_local! {
    static MINTED_TAGS: RefCell<Option<Vec<ReplyTag>>> = const { RefCell::new(None) };
}

#[cfg_attr(docsrs, doc(cfg(feature = "remote")))]
impl<R> Serialize for ReplyTo<R>
where
    R: DeserializeOwned + Send + 'static,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let Some(endpoint) = endpoint::get() else {
            return Err(ser::Error::custom(RefError::EndpointNotStarted));
        };

        let Some(send_reply) = self.take_send_reply() else {
            return Err(ser::Error::custom("reply destination already serialized"));
        };

        let nonce = endpoint
            .pending_replies()
            .add(Box::new(send_reply), deliver_reply::<R>);
        note_minted(ReplyTag {
            nonce,
            recipient: self.recipient(),
        });

        WireReplyTo {
            node: endpoint.node(),
            nonce,
            recipient: self.recipient(),
        }
        .serialize(serializer)
    }
}

#[cfg_attr(docsrs, doc(cfg(feature = "remote")))]
impl<'de, R> Deserialize<'de> for ReplyTo<R>
where
    R: Serialize + Send + 'static,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let WireReplyTo {
            node,
            nonce,
            recipient,
        } = WireReplyTo::deserialize(deserializer)?;

        let Some(endpoint) = endpoint::get() else {
            return Err(de::Error::custom(RefError::EndpointNotStarted));
        };

        if node == endpoint.node() {
            let Some(pending) = endpoint.pending_replies().take(nonce) else {
                return Err(de::Error::custom(
                    "no pending reply destination under this nonce",
                ));
            };
            let send_reply = pending
                .send_reply
                .downcast::<SendReply<R>>()
                .map_err(|_| de::Error::custom("reply destination of another reply type"))?;
            Ok(ReplyTo::new(recipient, *send_reply))
        } else {
            Ok(remote_proxy(node, nonce, recipient))
        }
    }
}

/// The reply destinations serialized away, awaiting a reply frame by nonce; dropping an entry is
/// what resolves its ask as `NoReply`. There is no internal timeout: the mandatory ask timeout is
/// the backstop for every loss this table cannot observe.
pub(crate) struct PendingReplies {
    next_nonce: AtomicU64,
    pending: Mutex<HashMap<Nonce, PendingReply>>,
}

impl PendingReplies {
    pub(crate) fn new() -> Self {
        Self {
            next_nonce: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Stamp the given entries with the peer their frame was sent to, arming their eviction upon
    /// that node's death or its lane's give-up.
    pub(crate) fn stamp(&self, tags: &[ReplyTag], peer: NodeId) {
        if tags.is_empty() {
            return;
        }

        let mut pending = lock(&self.pending);
        for tag in tags {
            if let Some(entry) = pending.get_mut(&tag.nonce) {
                entry.peer = Some(peer);
            }
        }
    }

    /// Drop the given entries: their frame never left, so nobody owes them a reply.
    pub(crate) fn discard(&self, tags: &[ReplyTag]) {
        if tags.is_empty() {
            return;
        }

        let mut pending = lock(&self.pending);
        for tag in tags {
            pending.remove(&tag.nonce);
        }
    }

    /// Dropping every entry stamped with the dead node is what resolves their asks as `NoReply`.
    pub(crate) fn fail_peer(&self, peer: NodeId) {
        lock(&self.pending).retain(|_, entry| entry.peer != Some(peer));
    }

    /// A given-up lane owes its peers nothing anymore: dropping the entries stamped with any
    /// incarnation at its address resolves their asks as `NoReply`, like node death does; a reply
    /// a live but unreachable peer sends later dies against the evicted entry, never behind the
    /// `NoReply`.
    pub(crate) fn fail_addr(&self, addr: SocketAddr) {
        lock(&self.pending).retain(|_, entry| entry.peer.is_none_or(|peer| peer.addr() != addr));
    }

    fn add(&self, send_reply: Box<dyn Any + Send>, deliver: DeliverReply) -> Nonce {
        let nonce = Nonce::mint(&self.next_nonce);

        lock(&self.pending).insert(
            nonce,
            PendingReply {
                peer: None,
                send_reply,
                deliver,
            },
        );
        nonce
    }

    fn take(&self, nonce: Nonce) -> Option<PendingReply> {
        lock(&self.pending).remove(&nonce)
    }
}

/// Names a reply destination embedded in a message payload, repeated in the frame next to the
/// payload: a node which dead-letters the message undecoded still learns which destinations it
/// carried and answers each with `ReplyDropped`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct ReplyTag {
    pub(crate) nonce: Nonce,
    pub(crate) recipient: ActorId,
}

/// Run the given function recording every reply destination minted on this thread, so a send
/// site learns which entries an encode registered: stamped on a successful send, discarded on a
/// failed one, and named in the frame for the receiving node's dead letter answer.
pub(crate) fn record_minted<T, F>(f: F) -> (T, Vec<ReplyTag>)
where
    F: FnOnce() -> T,
{
    /// Must restore the outer recording even if `f` unwinds!
    struct Restore {
        outer: Option<Vec<ReplyTag>>,
    }

    impl Drop for Restore {
        fn drop(&mut self) {
            MINTED_TAGS.set(self.outer.take());
        }
    }

    let restore = Restore {
        outer: MINTED_TAGS.replace(Some(Vec::new())),
    };
    let value = f();
    let tags = MINTED_TAGS.replace(None).unwrap_or_default();
    drop(restore);

    (value, tags)
}

pub(crate) fn on_reply(endpoint: &EndpointInner, peer: NodeId, nonce: Nonce, payload: &[u8]) {
    match endpoint.pending_replies().take(nonce) {
        Some(pending) => {
            if let Err(error) = (pending.deliver)(pending.send_reply, payload, endpoint.codec()) {
                warn!(%peer, %nonce, %error, "dead letter");
            }
        }

        None => warn!(
            %peer,
            %nonce,
            error = "no pending reply destination under this nonce",
            "dead letter"
        ),
    }
}

/// Dropping the entry is what resolves its ask as `NoReply`.
pub(crate) fn on_reply_dropped(endpoint: &EndpointInner, nonce: Nonce) {
    endpoint.pending_replies().take(nonce);
}

/// For a message dead-lettered undecoded only: a delivered payload's proxies answer for
/// themselves when dropped, and a partially decoded one already has, which a repeated
/// notification must tolerate; [on_reply_dropped] takes at most once, so it does.
pub(crate) fn on_undeliverable(endpoint: &EndpointInner, peer: NodeId, reply_tags: &[ReplyTag]) {
    for tag in reply_tags {
        let frame = Frame::ReplyDropped {
            nonce: tag.nonce,
            recipient: tag.recipient,
        };
        if let Err(error) = endpoint.send(peer, frame) {
            debug!(%peer, nonce = %tag.nonce, %error, "cannot send reply-dropped notification");
        }
    }
}

struct PendingReply {
    peer: Option<NodeId>,
    send_reply: Box<dyn Any + Send>,
    deliver: DeliverReply,
}

/// Sends a reply-dropped notification from `drop`; a proxy which does send its reply forgets the
/// guard instead, since the reply supersedes the notification.
struct ReplyGuard {
    origin: NodeId,
    nonce: Nonce,
    recipient: ActorId,
}

impl Drop for ReplyGuard {
    fn drop(&mut self) {
        let Some(endpoint) = endpoint::get() else {
            return;
        };

        let frame = Frame::ReplyDropped {
            nonce: self.nonce,
            recipient: self.recipient,
        };
        if let Err(error) = endpoint.send(self.origin, frame) {
            debug!(
                origin = %self.origin,
                nonce = %self.nonce,
                %error,
                "cannot send reply-dropped notification"
            );
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct WireReplyTo {
    node: NodeId,
    nonce: Nonce,
    recipient: ActorId,
}

fn deliver_reply<R>(
    send_reply: Box<dyn Any + Send>,
    payload: &[u8],
    codec: &dyn Codec,
) -> Result<(), CodecError>
where
    R: DeserializeOwned + Send + 'static,
{
    let send_reply = send_reply
        .downcast::<SendReply<R>>()
        .expect("the entry was added with this reply type's sink");
    let reply = codec.decode_to::<R>(payload)?;
    send_reply(reply);
    Ok(())
}

fn note_minted(tag: ReplyTag) {
    MINTED_TAGS.with_borrow_mut(|tags| {
        if let Some(tags) = tags {
            tags.push(tag);
        }
    });
}

fn remote_proxy<R>(origin: NodeId, nonce: Nonce, recipient: ActorId) -> ReplyTo<R>
where
    R: Serialize + Send + 'static,
{
    let guard = ReplyGuard {
        origin,
        nonce,
        recipient,
    };

    ReplyTo::new(recipient, move |reply| {
        match send_reply_frame(&reply, origin, nonce, recipient) {
            Ok(()) => mem::forget(guard),

            Err(error) => warn!(
                origin = %origin,
                recipient = %recipient,
                reply_type = type_name::<R>(),
                %error,
                "dead letter"
            ),
        }
    })
}

fn send_reply_frame<R>(
    reply: &R,
    origin: NodeId,
    nonce: Nonce,
    recipient: ActorId,
) -> Result<(), RemoteSendError>
where
    R: Serialize,
{
    let endpoint = endpoint::get().ok_or(RemoteSendError::EndpointNotStarted)?;

    let (payload, reply_tags) = record_minted(|| endpoint.codec().encode(reply));
    let sent = payload
        .map_err(RemoteSendError::from)
        .and_then(|payload| admit_payload(payload, endpoint.config().max_frame_size.get()))
        .and_then(|payload| {
            endpoint
                .send(
                    origin,
                    Frame::Reply {
                        nonce,
                        recipient,
                        payload: Cow::Owned(payload),
                    },
                )
                .map_err(RemoteSendError::from)
        });

    match sent {
        Ok(()) => {
            endpoint.pending_replies().stamp(&reply_tags, origin);
            Ok(())
        }

        Err(error) => {
            endpoint.pending_replies().discard(&reply_tags);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ActorId,
        remote::{
            codec::{Codec, CodecError},
            discovery::Nonce,
            node::NodeId,
            reply::{PendingReplies, ReplyTag, note_minted, record_minted},
        },
    };
    use std::any::Any;

    fn tag(nonce: Nonce) -> ReplyTag {
        ReplyTag {
            nonce,
            recipient: ActorId::new(),
        }
    }

    /// An entry is taken exactly once; a second take under the same nonce finds nothing, which is
    /// also what makes a repeated reply-dropped notification for one entry a no-op.
    #[test]
    fn entries_are_taken_at_most_once() {
        let pending = PendingReplies::new();

        let nonce = pending.add(Box::new(()), deliver_nothing);

        assert!(pending.take(nonce).is_some());
        assert!(pending.take(nonce).is_none());
    }

    /// Node death only evicts the entries stamped with the dead node: an unstamped entry (its
    /// frame not sent yet) and one stamped with another node survive.
    #[test]
    fn fail_peer_evicts_only_matching_stamps() {
        let addr = "127.0.0.1:1234".parse().expect("valid address");
        let peer = NodeId::new(addr);
        let other = NodeId::new(addr);
        let pending = PendingReplies::new();

        let stamped = pending.add(Box::new(()), deliver_nothing);
        let other_stamped = pending.add(Box::new(()), deliver_nothing);
        let unstamped = pending.add(Box::new(()), deliver_nothing);
        pending.stamp(&[tag(stamped)], peer);
        pending.stamp(&[tag(other_stamped)], other);

        pending.fail_peer(peer);

        assert!(pending.take(stamped).is_none());
        assert!(pending.take(other_stamped).is_some());
        assert!(pending.take(unstamped).is_some());
    }

    /// Giving up a lane evicts the entries stamped with any incarnation at its address, so their
    /// asks resolve as `NoReply` instead of waiting out their timeout; an unstamped entry and one
    /// stamped with another address survive.
    #[test]
    fn fail_addr_evicts_every_incarnation_at_the_address() {
        let addr = "127.0.0.1:1234".parse().expect("valid address");
        let other_addr = "127.0.0.1:5678".parse().expect("valid address");
        let pending = PendingReplies::new();

        let stamped = pending.add(Box::new(()), deliver_nothing);
        let successor_stamped = pending.add(Box::new(()), deliver_nothing);
        let other_stamped = pending.add(Box::new(()), deliver_nothing);
        let unstamped = pending.add(Box::new(()), deliver_nothing);
        pending.stamp(&[tag(stamped)], NodeId::new(addr));
        pending.stamp(&[tag(successor_stamped)], NodeId::new(addr));
        pending.stamp(&[tag(other_stamped)], NodeId::new(other_addr));

        pending.fail_addr(addr);

        assert!(pending.take(stamped).is_none());
        assert!(pending.take(successor_stamped).is_none());
        assert!(pending.take(other_stamped).is_some());
        assert!(pending.take(unstamped).is_some());
    }

    /// Discarding removes exactly the given entries, e.g. after a failed send.
    #[test]
    fn discard_removes_the_given_entries() {
        let pending = PendingReplies::new();

        let discarded = pending.add(Box::new(()), deliver_nothing);
        let kept = pending.add(Box::new(()), deliver_nothing);

        pending.discard(&[tag(discarded)]);

        assert!(pending.take(discarded).is_none());
        assert!(pending.take(kept).is_some());
    }

    /// A nested recording keeps its tags to itself and restores the outer one, so a send during
    /// an encode cannot corrupt the outer send's bookkeeping.
    #[test]
    fn record_minted_scopes_nest() {
        let ((), outer) = record_minted(|| {
            note_minted(tag(Nonce::first()));

            let ((), inner) = record_minted(|| note_minted(tag(Nonce::first())));
            assert_eq!(inner.len(), 1);

            note_minted(tag(Nonce::first()));
        });

        assert_eq!(outer.len(), 2);
    }

    fn deliver_nothing(
        _send_reply: Box<dyn Any + Send>,
        _payload: &[u8],
        _codec: &dyn Codec,
    ) -> Result<(), CodecError> {
        Ok(())
    }
}
