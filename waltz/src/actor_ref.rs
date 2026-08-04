use crate::{
    ActorId, MailboxCapacity,
    mailbox::{Mailbox, MailboxHandle, TerminatedSink, Watcher, WatcherRegistry, make_mailbox},
};
use derive_more::Debug;
use log::warn;
use std::{
    any::type_name,
    fmt::Display,
    hash::{Hash, Hasher},
    sync::Arc,
};

/// A shareable reference to an actor, used to send it messages and read its ID.
///
/// Equality and hashing are by [ActorRef::actor_id] alone: any two references naming the same actor
/// compare equal and hash equally, however they were obtained.
#[derive(Debug)]
pub struct ActorRef<M> {
    actor_id: ActorId,

    #[debug(skip)]
    mailbox_handle: MailboxHandle<M>,
}

impl<M> ActorRef<M> {
    /// The ID of the actor represented by this reference.
    pub fn actor_id(&self) -> ActorId {
        self.actor_id
    }

    /// Send a message to the actor represented by this reference without blocking. If the actor
    /// has terminated, or if its mailbox is full for [crate::MailboxCapacity::Bounded], the message
    /// is dropped and logged as a dead letter. Also, even if the message is delivered to the
    /// actor, it might stop before processing it.
    pub fn tell(&self, message: M) {
        if let Err(error) = self.mailbox_handle.try_send_message(message) {
            self.dead_letter(&error);
        }
    }

    pub(crate) fn watcher_registry(&self) -> &WatcherRegistry {
        self.mailbox_handle.watcher_registry()
    }

    fn new(actor_id: ActorId, mailbox_handle: MailboxHandle<M>) -> Self {
        Self {
            actor_id,
            mailbox_handle,
        }
    }

    fn dead_letter(&self, error: &dyn Display) {
        warn!(
            actor_id:% = self.actor_id,
            message_type = type_name::<M>(),
            error:%;
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
            mailbox_handle: self.mailbox_handle.clone(),
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
