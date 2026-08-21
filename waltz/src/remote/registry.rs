use crate::{
    ActorId, ActorRef,
    mailbox::{ActorTerminated, TerminatedSink, Watcher, WatcherRegistry},
    remote::{
        codec::{Codec, CodecError},
        discovery::WireKey,
    },
    sync::{lock, read, write},
};
use serde::de::DeserializeOwned;
use std::{
    any::{self, Any},
    collections::{HashMap, HashSet, hash_map::Entry},
    sync::{Arc, Mutex, RwLock, Weak},
};
use thiserror::Error;

/// An actor is bound lazily when a reference to it is serialized and evicted once it has
/// terminated. Names are a second keyspace over the same bindings, so discovery and reference
/// serialization reach the same routes and are evicted together. The routes are behind a
/// [RwLock]: every inbound delivery reads them, from one reader task per stream per peer, and
/// only binding and eviction write.
pub(crate) struct Registry {
    routes: RwLock<HashMap<ActorId, Arc<dyn InboundRoute>>>,
    names: Mutex<Names>,
}

impl Registry {
    pub(crate) fn new() -> Self {
        Self {
            routes: RwLock::new(HashMap::new()),
            names: Mutex::new(Names::new()),
        }
    }

    /// Idempotent; a binding for an already terminated actor is reverted right away. The registry
    /// lock must be released before registering the evictor: the revert path takes it again and the
    /// mutex is not reentrant.
    pub(crate) fn bind<M>(self: &Arc<Self>, actor_ref: &ActorRef<M>)
    where
        M: DeserializeOwned + Send + 'static,
    {
        let id = actor_ref.actor_id();

        let bound = match write(&self.routes).entry(id) {
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

        let evictor = Watcher::new(
            ActorId::new(),
            Arc::new(Evictor {
                registry: Arc::downgrade(self),
            }),
        );
        let watcher_registry = actor_ref
            .watcher_registry()
            .expect("bound reference is local");
        if watcher_registry.add(evictor).is_err() {
            self.unbind(id);
        }
    }

    /// Name before bind, so an actor which has already terminated takes its name down with it: the
    /// revert inside [Registry::bind] unbinds, and unbinding drops the names.
    pub(crate) fn register<M>(self: &Arc<Self>, name: String, actor_ref: &ActorRef<M>)
    where
        M: DeserializeOwned + Send + 'static,
    {
        lock(&self.names).add(name, actor_ref.actor_id(), any::type_name::<M>());
        self.bind(actor_ref);
    }

    pub(crate) fn named(&self, key: &WireKey) -> Named {
        lock(&self.names).get(&key.name, &key.type_tag)
    }

    pub(crate) fn deliver(
        &self,
        target: ActorId,
        payload: &[u8],
        codec: &dyn Codec,
    ) -> Result<(), DeliverError> {
        let route = read(&self.routes).get(&target).cloned();

        route
            .ok_or(DeliverError::UnknownTarget)?
            .deliver(payload, codec)?;

        Ok(())
    }

    pub(crate) fn local_ref<M>(&self, id: ActorId) -> Result<ActorRef<M>, LocalRefError>
    where
        M: Send + 'static,
    {
        let route = read(&self.routes)
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
        let route = read(&self.routes).get(&id).cloned()?;
        Some(route.watcher_registry())
    }

    /// The two locks are taken one after the other, never nested, so a name registration and an
    /// eviction cannot deadlock each other.
    pub(crate) fn unbind(&self, id: ActorId) {
        write(&self.routes).remove(&id);
        lock(&self.names).remove(id);
    }
}

#[derive(Debug, Error)]
pub(crate) enum DeliverError {
    #[error("unknown target actor")]
    UnknownTarget,

    #[error(transparent)]
    Codec(#[from] CodecError),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Named {
    Found(ActorId),
    NotFound,
    TypeMismatch,
}

#[derive(Debug, Error)]
pub(crate) enum LocalRefError {
    #[error("no actor bound under this ID")]
    Unbound,

    #[error("actor is bound for a different message type")]
    MessageType,
}

/// Keyed both ways: by name to answer a lookup, by actor to evict a terminated actor's names
/// without scanning every name ever registered.
struct Names {
    by_name: HashMap<String, HashMap<ActorId, &'static str>>,
    by_id: HashMap<ActorId, HashSet<String>>,
}

impl Names {
    fn new() -> Self {
        Self {
            by_name: HashMap::new(),
            by_id: HashMap::new(),
        }
    }

    fn add(&mut self, name: String, id: ActorId, type_tag: &'static str) {
        self.by_name
            .entry(name.clone())
            .or_default()
            .insert(id, type_tag);
        self.by_id.entry(id).or_default().insert(name);
    }

    /// A name registered for another message type is not a miss: telling the two apart is what
    /// turns a mismatched build into an error rather than into messages nobody can decode.
    fn get(&self, name: &str, type_tag: &str) -> Named {
        let Some(registered) = self.by_name.get(name) else {
            return Named::NotFound;
        };

        match registered
            .iter()
            .find(|(_, registered)| **registered == type_tag)
        {
            Some((id, _)) => Named::Found(*id),
            None => Named::TypeMismatch,
        }
    }

    fn remove(&mut self, id: ActorId) {
        let Some(names) = self.by_id.remove(&id) else {
            return;
        };

        for name in names {
            let Some(registered) = self.by_name.get_mut(&name) else {
                continue;
            };

            registered.remove(&id);
            if registered.is_empty() {
                self.by_name.remove(&name);
            }
        }
    }
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
        let message = codec.decode_to::<M>(payload)?;
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
/// unbind. Holds its registry weakly: a dropped registry means there is nothing to evict from.
struct Evictor {
    registry: Weak<Registry>,
}

impl TerminatedSink for Evictor {
    fn send_terminated(&self, actor_id: ActorId) -> Result<(), ActorTerminated> {
        if let Some(registry) = self.registry.upgrade() {
            registry.unbind(actor_id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ActorId, MailboxCapacity,
        actor_ref::SelfRef,
        remote::{
            discovery::WireKey,
            registry::{LocalRefError, Named, Registry},
        },
    };
    use std::{
        sync::{Arc, mpsc},
        thread,
        time::Duration,
    };

    const TIMEOUT: Duration = Duration::from_secs(5);

    /// Binding an actor which has already terminated reverts the binding, which takes the
    /// registry lock again: the bind path must have released it, else the endpoint deadlocks.
    /// Run on its own thread, so a regression fails here instead of hanging the whole suite.
    #[test]
    fn binding_a_terminated_actor_does_not_deadlock() {
        let (done_tx, done_rx) = mpsc::channel();

        thread::spawn(move || {
            let registry = Arc::new(Registry::new());

            let id = ActorId::new();
            let (self_ref, mailbox) = SelfRef::<u32>::new(id, MailboxCapacity::Unbounded);
            let actor_ref = self_ref.actor_ref().clone();

            mailbox.split().1.take_watchers();

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
        let registry = Arc::new(Registry::new());

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
        let registry = Arc::new(Registry::new());

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

    /// Termination evicts through the watcher mechanism itself: taking the watchers and signaling
    /// them, as the run loop does, must remove both the route and the name. This is the path the
    /// direct `unbind` tests below cannot cover.
    #[test]
    fn termination_evicts_route_and_name() {
        let registry = Arc::new(Registry::new());

        let id = ActorId::new();
        let (self_ref, mailbox) = SelfRef::<u32>::new(id, MailboxCapacity::Unbounded);
        registry.register("pool".to_string(), self_ref.actor_ref());

        for watcher in mailbox.split().1.take_watchers() {
            watcher
                .send_terminated(id)
                .expect("the evictor accepts the signal");
        }

        assert!(matches!(
            registry.local_ref::<u32>(id),
            Err(LocalRefError::Unbound)
        ));
        assert_eq!(
            registry.named(&WireKey::new::<u32>("pool")),
            Named::NotFound
        );
    }

    /// Eviction removes the route, so a terminated actor stops being reachable from other nodes.
    #[test]
    fn unbinding_removes_the_route() {
        let registry = Arc::new(Registry::new());

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

    /// A name resolves only together with the message type it was registered for: a lookup naming
    /// another type is a mismatch rather than a miss, so a build skew is an error rather than
    /// messages nobody can decode.
    #[test]
    fn a_name_resolves_for_its_message_type_only() {
        let registry = Arc::new(Registry::new());

        let id = ActorId::new();
        let (self_ref, _mailbox) = SelfRef::<u32>::new(id, MailboxCapacity::Unbounded);
        registry.register("pool".to_string(), self_ref.actor_ref());

        assert_eq!(
            registry.named(&WireKey::new::<u32>("pool")),
            Named::Found(id)
        );
        assert_eq!(
            registry.named(&WireKey::new::<String>("pool")),
            Named::TypeMismatch
        );
        assert_eq!(
            registry.named(&WireKey::new::<u32>("other")),
            Named::NotFound
        );
    }

    /// A name is registered on the same binding as a serialized reference, so eviction takes both:
    /// a terminated actor is neither reachable by ID nor discoverable by name.
    #[test]
    fn unbinding_removes_the_name() {
        let registry = Arc::new(Registry::new());

        let id = ActorId::new();
        let (self_ref, _mailbox) = SelfRef::<u32>::new(id, MailboxCapacity::Unbounded);
        registry.register("pool".to_string(), self_ref.actor_ref());

        registry.unbind(id);

        assert_eq!(
            registry.named(&WireKey::new::<u32>("pool")),
            Named::NotFound
        );
    }

    /// More than one actor may answer to a name, and losing one leaves the others discoverable:
    /// the name keyspace is the substrate a receptionist listing would grow from.
    #[test]
    fn a_name_takes_more_than_one_actor() {
        let registry = Arc::new(Registry::new());

        let (first, second) = (ActorId::new(), ActorId::new());
        let (first_ref, _first_mailbox) = SelfRef::<u32>::new(first, MailboxCapacity::Unbounded);
        let (second_ref, _second_mailbox) = SelfRef::<u32>::new(second, MailboxCapacity::Unbounded);
        registry.register("pool".to_string(), first_ref.actor_ref());
        registry.register("pool".to_string(), second_ref.actor_ref());

        registry.unbind(first);

        assert_eq!(
            registry.named(&WireKey::new::<u32>("pool")),
            Named::Found(second)
        );
    }
}
