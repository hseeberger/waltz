use crate::{
    ActorId, AskError, MailboxCapacity, ReplyTo,
    mailbox::{Mailbox, MailboxHandle, TerminatedSink, Watcher, WatcherRegistry, make_mailbox},
};
use derive_more::Debug;
use std::{
    any::type_name,
    fmt::Display,
    hash::{Hash, Hasher},
    sync::Arc,
    time::Duration,
};
use tokio::{sync::oneshot, time::timeout};
use tracing::warn;

/// A shareable reference to an actor, used to send it messages and read its ID.
///
/// Equality and hashing are by [ActorRef::actor_id] alone: any two references naming the same actor
/// compare equal and hash equally, however they were obtained.
#[derive(Debug)]
pub struct ActorRef<M> {
    actor_id: ActorId,

    #[debug(skip)]
    sink: Sink<M>,
}

impl<M> ActorRef<M> {
    /// The ID of the actor represented by this reference.
    pub fn actor_id(&self) -> ActorId {
        self.actor_id
    }

    /// Send a message to the actor represented by this reference without blocking. If the actor
    /// has terminated, or if its mailbox is full for [crate::MailboxCapacity::Bounded], the message
    /// is dropped and logged as a dead letter. Also, even if the message is delivered to the
    /// actor, it might stop before processing it. With the `remote` feature a reference can point
    /// to an actor on another node; then an unreachable node or a full outbound queue equally
    /// makes the message a dead letter.
    pub fn tell(&self, message: M) {
        match &self.sink {
            Sink::Local(mailbox_handle) => {
                if let Err(error) = mailbox_handle.try_send_message(message) {
                    self.dead_letter(&error);
                }
            }

            #[cfg(feature = "remote")]
            Sink::Remote(remote_sink) => {
                if let Err(error) = remote_sink.try_send_message(message) {
                    self.dead_letter(&error);
                }
            }
        }
    }

    /// Send a request to the actor represented by this reference and await the reply for at most
    /// `within`. The given function builds the request message around a [ReplyTo] for the actor
    /// to [ReplyTo::reply] to.
    ///
    /// Unlike [ActorRef::tell], failures are returned instead of only logged, since the caller is
    /// awaiting: [AskError::MailboxFull] and [AskError::ActorTerminated] if the request cannot be
    /// sent, [AskError::NoReply] once it is detected that no reply can arrive anymore and
    /// [AskError::Timeout] once `within` has elapsed without a reply. The `NoReply` detection is
    /// best-effort, which is why the wait is bounded: against e.g. a responder which keeps its
    /// [ReplyTo] alive without replying, the ask resolves as `Timeout`; a late reply is dropped
    /// and logged as a dead letter.
    ///
    /// With the `remote` feature the reference can point to an actor on another node; the same
    /// contract holds, with the `NoReply` detection weakening as spelled out in the `remote`
    /// module documentation.
    ///
    /// For code outside of any actor, e.g. alongside [ActorSystem::terminated]; inside an actor
    /// use [ActorContext::reply_to] instead of awaiting.
    ///
    /// [ActorContext::reply_to]: crate::ActorContext::reply_to
    /// [ActorSystem::terminated]: crate::ActorSystem::terminated
    pub async fn ask<R, F>(&self, within: Duration, make_message: F) -> Result<R, AskError>
    where
        F: FnOnce(ReplyTo<R>) -> M,
        R: Send + 'static,
    {
        let actor_id = self.actor_id;
        let (reply_tx, reply_rx) = oneshot::channel();

        let reply_to = ReplyTo::new(actor_id, move |reply| {
            if reply_tx.send(reply).is_err() {
                warn!(
                    %actor_id,
                    reply_type = type_name::<R>(),
                    error = "asker no longer awaits the reply",
                    "dead letter"
                );
            }
        });

        match &self.sink {
            Sink::Local(mailbox_handle) => {
                mailbox_handle.try_send_message(make_message(reply_to))?
            }

            #[cfg(feature = "remote")]
            Sink::Remote(remote_sink) => remote_sink.try_send_message(make_message(reply_to))?,
        }

        match timeout(within, reply_rx).await {
            Ok(reply) => reply.map_err(|_| AskError::NoReply),
            Err(_) => Err(AskError::Timeout(within)),
        }
    }

    pub(crate) fn watch_target(&self) -> WatchTarget<'_> {
        match &self.sink {
            Sink::Local(mailbox_handle) => WatchTarget::Local(mailbox_handle.watcher_registry()),

            #[cfg(feature = "remote")]
            Sink::Remote(remote_sink) => WatchTarget::Remote(remote_sink.node()),
        }
    }

    #[cfg(feature = "remote")]
    pub(crate) fn watcher_registry(&self) -> Option<&WatcherRegistry> {
        match &self.sink {
            Sink::Local(mailbox_handle) => Some(mailbox_handle.watcher_registry()),
            Sink::Remote(_) => None,
        }
    }

    #[cfg(feature = "remote")]
    pub(crate) fn remote(remote_sink: crate::remote::RemoteSink<M>) -> Self {
        Self {
            actor_id: remote_sink.target(),
            sink: Sink::Remote(remote_sink),
        }
    }

    fn new(actor_id: ActorId, mailbox_handle: MailboxHandle<M>) -> Self {
        Self {
            actor_id,
            sink: Sink::Local(mailbox_handle),
        }
    }

    fn dead_letter(&self, error: &dyn Display) {
        warn!(
            actor_id = %self.actor_id,
            message_type = type_name::<M>(),
            %error,
            "dead letter"
        );
    }
}

// Derived impls would needlessly require `M: PartialEq`/`M: Hash`.
impl<M> PartialEq for ActorRef<M> {
    fn eq(&self, other: &Self) -> bool {
        self.actor_id == other.actor_id
    }
}

impl<M> Eq for ActorRef<M> {}

impl<M> Hash for ActorRef<M> {
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.actor_id.hash(state);
    }
}

// A derived `Clone` would needlessly require `M: Clone`.
impl<M> Clone for ActorRef<M> {
    fn clone(&self) -> Self {
        Self {
            actor_id: self.actor_id,
            sink: self.sink.clone(),
        }
    }
}

pub(crate) enum WatchTarget<'a> {
    Local(&'a WatcherRegistry),

    #[cfg(feature = "remote")]
    Remote(crate::remote::NodeId),
}

pub(crate) enum Sink<M> {
    Local(MailboxHandle<M>),

    #[cfg(feature = "remote")]
    Remote(crate::remote::RemoteSink<M>),
}

// A derived `Clone` would needlessly require `M: Clone`.
impl<M> Clone for Sink<M> {
    fn clone(&self) -> Self {
        match self {
            Sink::Local(mailbox_handle) => Sink::Local(mailbox_handle.clone()),

            #[cfg(feature = "remote")]
            Sink::Remote(remote_sink) => Sink::Remote(remote_sink.clone()),
        }
    }
}

#[derive(Debug)]
pub(crate) struct SelfRef<M> {
    actor_ref: ActorRef<M>,

    #[debug(skip)]
    terminated_sink: Arc<dyn TerminatedSink>,
}

impl<M> SelfRef<M> {
    pub(crate) fn new(actor_id: ActorId, mailbox_capacity: MailboxCapacity) -> (Self, Mailbox<M>)
    where
        M: Send + 'static,
    {
        let (mailbox_handle, mailbox) = make_mailbox(mailbox_capacity);
        let terminated_sink = mailbox_handle.terminated_sink();
        let actor_ref = ActorRef::new(actor_id, mailbox_handle);

        (
            Self {
                actor_ref,
                terminated_sink,
            },
            mailbox,
        )
    }

    pub(crate) fn actor_ref(&self) -> &ActorRef<M> {
        &self.actor_ref
    }

    pub(crate) fn make_watcher(&self) -> Watcher {
        Watcher::new(self.actor_ref.actor_id, self.terminated_sink.clone())
    }

    pub(crate) fn send_terminated(&self, actor_id: ActorId) {
        self.terminated_sink
            .send_terminated(actor_id)
            .expect("the actor's own mailbox outlives its context");
    }
}

// A derived `Clone` would needlessly require `M: Clone`.
impl<M> Clone for SelfRef<M> {
    fn clone(&self) -> Self {
        Self {
            actor_ref: self.actor_ref.clone(),
            terminated_sink: self.terminated_sink.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{ActorId, ActorRef, MailboxCapacity, actor_ref::SelfRef};
    use std::hash::{DefaultHasher, Hash, Hasher};

    /// References compare and hash by ID alone, hence a clone stands in for the original wherever
    /// references are used as keys, while a reference to another actor does not.
    #[test]
    fn references_are_equal_and_hash_by_id() {
        let (self_ref, _mailbox) = SelfRef::<()>::new(ActorId::new(), MailboxCapacity::Unbounded);
        let (other_ref, _other_mailbox) =
            SelfRef::<()>::new(ActorId::new(), MailboxCapacity::Unbounded);
        let actor_ref = self_ref.actor_ref().clone();

        assert_eq!(&actor_ref, self_ref.actor_ref());
        assert_eq!(hash_of(&actor_ref), hash_of(self_ref.actor_ref()));

        assert_ne!(&actor_ref, other_ref.actor_ref());
    }

    fn hash_of<M>(actor_ref: &ActorRef<M>) -> u64 {
        let mut hasher = DefaultHasher::new();
        actor_ref.hash(&mut hasher);
        hasher.finish()
    }
}
