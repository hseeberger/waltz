use crate::{
    ActorId, ActorRef,
    mailbox::{ActorTerminated, TerminatedSink, Watcher, WatcherRegistry},
    remote::{
        codec::{Codec, CodecError},
        endpoint,
    },
    sync::lock,
};
use serde::de::DeserializeOwned;
use std::{
    any::Any,
    collections::{HashMap, hash_map::Entry},
    sync::{Arc, Mutex},
};
use thiserror::Error;

/// An actor is bound lazily when a reference to it is serialized and evicted once it has
/// terminated.
pub(crate) struct Registry(Mutex<HashMap<ActorId, Arc<dyn InboundRoute>>>);

impl Registry {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
    }

    /// Idempotent; a binding for an already terminated actor is reverted right away. The registry
    /// lock must be released before registering the evictor: the revert path takes it again and the
    /// mutex is not reentrant.
    pub(crate) fn bind<M>(&self, actor_ref: &ActorRef<M>)
    where
        M: DeserializeOwned + Send + 'static,
    {
        let id = actor_ref.actor_id();

        let bound = match lock(&self.0).entry(id) {
            Entry::Vacant(entry) => {
                entry.insert(Arc::new(Route {
                    actor_ref: actor_ref.clone(),
                }));
                true
            }

            Entry::Occupied(_) => false,
        };
        if !bound {
            return;
        }

        let evictor = Watcher::new(ActorId::new(), Arc::new(Evictor));
        let watcher_registry = actor_ref
            .watcher_registry()
            .expect("bound reference is local");
        if watcher_registry.add(evictor).is_err() {
            self.unbind(id);
        }
    }

    pub(crate) fn deliver(
        &self,
        target: ActorId,
        payload: &[u8],
        codec: &dyn Codec,
    ) -> Result<(), DeliverError> {
        let route = lock(&self.0).get(&target).cloned();

        route
            .ok_or(DeliverError::UnknownTarget)?
            .deliver(payload, codec)
            .map_err(DeliverError::Codec)
    }

    pub(crate) fn local_ref<M>(&self, id: ActorId) -> Result<ActorRef<M>, LocalRefError>
    where
        M: Send + 'static,
    {
        let route = lock(&self.0)
            .get(&id)
            .cloned()
            .ok_or(LocalRefError::Unbound)?;

        route
            .as_any()
            .downcast_ref::<Route<M>>()
            .map(|route| route.actor_ref.clone())
            .ok_or(LocalRefError::MessageType)
    }

    pub(crate) fn watcher_registry(&self, id: ActorId) -> Option<WatcherRegistry> {
        let route = lock(&self.0).get(&id).cloned()?;
        Some(route.watcher_registry())
    }

    pub(crate) fn unbind(&self, id: ActorId) {
        lock(&self.0).remove(&id);
    }
}

#[derive(Debug, Error)]
pub(crate) enum DeliverError {
    #[error("unknown target actor")]
    UnknownTarget,

    #[error(transparent)]
    Codec(#[from] CodecError),
}

#[derive(Debug, Error)]
pub(crate) enum LocalRefError {
    #[error("no actor bound under this ID")]
    Unbound,

    #[error("actor is bound for a different message type")]
    MessageType,
}

trait InboundRoute
where
    Self: Send + Sync,
{
    fn deliver(&self, payload: &[u8], codec: &dyn Codec) -> Result<(), CodecError>;

    fn watcher_registry(&self) -> WatcherRegistry;

    fn as_any(&self) -> &dyn Any;
}

struct Route<M> {
    actor_ref: ActorRef<M>,
}

impl<M> InboundRoute for Route<M>
where
    M: DeserializeOwned + Send + 'static,
{
    fn deliver(&self, payload: &[u8], codec: &dyn Codec) -> Result<(), CodecError> {
        let mut message = None;
        codec.decode(payload, &mut |deserializer| {
            message = Some(erased_serde::deserialize::<M>(deserializer)?);
            Ok(())
        })?;

        let message = message
            .ok_or_else(|| CodecError::decoding("codec did not decode a message".to_string()))?;
        self.actor_ref.tell(message);
        Ok(())
    }

    fn watcher_registry(&self) -> WatcherRegistry {
        self.actor_ref
            .watcher_registry()
            .expect("route target is local")
            .clone()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Registered on the very actor it is signaled about, hence the signaled ID is the one to
/// unbind.
struct Evictor;

impl TerminatedSink for Evictor {
    fn send_terminated(&self, actor_id: ActorId) -> Result<(), ActorTerminated> {
        if let Some(endpoint) = endpoint::get() {
            endpoint.registry().unbind(actor_id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ActorId, MailboxCapacity,
        actor_ref::SelfRef,
        remote::registry::{LocalRefError, Registry},
    };
    use std::{sync::mpsc, thread, time::Duration};

    const TIMEOUT: Duration = Duration::from_secs(5);

    /// Binding an actor which has already terminated reverts the binding, which takes the
    /// registry lock again: the bind path must have released it, else the endpoint deadlocks.
    /// Run on its own thread, so a regression fails here instead of hanging the whole suite.
    #[test]
    fn binding_a_terminated_actor_does_not_deadlock() {
        let (done_tx, done_rx) = mpsc::channel();

        thread::spawn(move || {
            let registry = Registry::new();

            let id = ActorId::new();
            let (self_ref, mailbox) = SelfRef::<u32>::new(id, MailboxCapacity::Unbounded);
            let actor_ref = self_ref.actor_ref().clone();

            mailbox.take_watchers();

            registry.bind(&actor_ref);
            let _ = done_tx.send(matches!(
                registry.local_ref::<u32>(id),
                Err(LocalRefError::Unbound)
            ));
        });

        match done_rx.recv_timeout(TIMEOUT) {
            Ok(reverted) => assert!(reverted, "binding a terminated actor was not reverted"),
            Err(_) => panic!("binding a terminated actor deadlocked"),
        }
    }

    /// Binding twice registers once and keeps the first route, so a second serialization of the
    /// same reference does not replace a live route.
    #[test]
    fn binding_twice_is_idempotent() {
        let registry = Registry::new();

        let id = ActorId::new();
        let (self_ref, _mailbox) = SelfRef::<u32>::new(id, MailboxCapacity::Unbounded);
        let actor_ref = self_ref.actor_ref().clone();

        registry.bind(&actor_ref);
        registry.bind(&actor_ref);

        assert!(registry.local_ref::<u32>(id).is_ok());
    }

    /// A bound reference is only handed back for the message type it was bound with: the downcast
    /// is what keeps a type mismatch from delivering garbage to an actor.
    #[test]
    fn local_ref_requires_the_bound_message_type() {
        let registry = Registry::new();

        let id = ActorId::new();
        let (self_ref, _mailbox) = SelfRef::<u32>::new(id, MailboxCapacity::Unbounded);
        let actor_ref = self_ref.actor_ref().clone();

        registry.bind(&actor_ref);

        assert!(registry.local_ref::<u32>(id).is_ok());
        assert!(matches!(
            registry.local_ref::<String>(id),
            Err(LocalRefError::MessageType)
        ));
        assert!(matches!(
            registry.local_ref::<u32>(ActorId::new()),
            Err(LocalRefError::Unbound)
        ));
    }

    /// Eviction removes the route, so a terminated actor stops being reachable from other nodes.
    #[test]
    fn unbinding_removes_the_route() {
        let registry = Registry::new();

        let id = ActorId::new();
        let (self_ref, _mailbox) = SelfRef::<u32>::new(id, MailboxCapacity::Unbounded);
        let actor_ref = self_ref.actor_ref().clone();

        registry.bind(&actor_ref);
        registry.unbind(id);

        assert!(matches!(
            registry.local_ref::<u32>(id),
            Err(LocalRefError::Unbound)
        ));
    }
}
