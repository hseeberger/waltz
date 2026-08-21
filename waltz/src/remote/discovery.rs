use crate::{
    ActorId, ActorRef,
    remote::{
        endpoint::{self, EndpointInner},
        frame::Frame,
        node::NodeId,
        registry::{LocalRefError, Named},
        wire,
    },
    sync::lock,
};
use derive_more::Display;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    any,
    collections::HashMap,
    fmt::{Debug, Formatter},
    marker::PhantomData,
    net::SocketAddr,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use thiserror::Error;
use tokio::sync::oneshot;
use tracing::warn;

/// The name an actor is registered under for discovery, typed by the actor's message type: the
/// type travels with the name, so a [lookup] naming the wrong one is refused rather than resolved
/// into a reference which drops every message it is told.
///
/// The type is compared by its name as both nodes' compilers spell it, which assumes they are
/// built from the same source. That is the practical case for the nodes of one system and the
/// same assumption the wire format already makes.
pub struct Key<M> {
    name: String,
    message: PhantomData<fn() -> M>,
}

impl<M> Key<M> {
    /// A key under the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            message: PhantomData,
        }
    }

    fn wire(&self) -> WireKey {
        WireKey::new::<M>(self.name.clone())
    }
}

impl<M> Debug for Key<M> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Key")
            .field("name", &self.name)
            .field("message", &any::type_name::<M>())
            .finish()
    }
}

// A derived `Clone` would needlessly require `M: Clone`.
impl<M> Clone for Key<M> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            message: PhantomData,
        }
    }
}

/// An actor which cannot be registered for discovery.
#[derive(Debug, Error)]
pub enum RegisterError {
    /// The remoting endpoint has not been started, see [start](crate::remote::start).
    #[error("remoting endpoint not started")]
    EndpointNotStarted,

    /// The reference names an actor on another node; only actors of this node can be registered
    /// here.
    #[error("reference names an actor on another node")]
    NotLocal,
}

/// A key which cannot be resolved.
#[derive(Debug, Error)]
pub enum LookupError {
    /// The remoting endpoint has not been started, see [start](crate::remote::start).
    #[error("remoting endpoint not started")]
    EndpointNotStarted,

    /// No actor could be resolved under this key: either the node answered that it has nothing
    /// registered, or the actor it named terminated before the reference was resolved. During
    /// bootstrap the former is the ordinary answer of a node whose actor is not registered *yet*,
    /// hence worth retrying.
    #[error("no actor registered under this key")]
    NotFound,

    /// The node has an actor registered under this name, but for another message type.
    #[error("actor registered under this key expects another message type")]
    TypeMismatch,

    /// The node could not be reached, or was given up on before it answered.
    #[error("node at {0} unreachable")]
    Unreachable(SocketAddr),
}

/// Register an actor of this node under a key, so other nodes can [lookup] it by name and address
/// instead of being handed a serialized reference out of band.
///
/// More than one actor may be registered under one key; a lookup then answers with one of them.
/// The registration is dropped once the actor terminates, along with the reference binding it
/// shares with reference serialization. Registering the same actor again under the same key
/// changes nothing.
pub fn register<M>(key: &Key<M>, actor_ref: &ActorRef<M>) -> Result<(), RegisterError>
where
    M: DeserializeOwned + Send + 'static,
{
    let endpoint = endpoint::get().ok_or(RegisterError::EndpointNotStarted)?;
    if actor_ref.watcher_registry().is_none() {
        return Err(RegisterError::NotLocal);
    }

    endpoint.registry().register(key.name.clone(), actor_ref);
    Ok(())
}

/// Resolve a key at the node advertising the given address, dialing it like any other message
/// would. The resolved reference names the incarnation which answered, so it stops working when
/// that node is replaced, exactly like one which travelled inside a message.
///
/// There is no timeout: a lookup towards a node which is up but silent waits. Wrap it in
/// [timeout](tokio::time::timeout) and retry [LookupError::NotFound] to bootstrap against a node
/// which may not have registered its actor yet.
pub async fn lookup<M>(key: &Key<M>, addr: SocketAddr) -> Result<ActorRef<M>, LookupError>
where
    M: Serialize + Send + 'static,
{
    let endpoint = endpoint::get().ok_or(LookupError::EndpointNotStarted)?;

    let key = key.wire();
    let (nonce, result_rx) = endpoint.pending_lookups().add(addr, key.clone());

    if let Err(error) = endpoint.send_to_addr(addr, Frame::Lookup { nonce, key }) {
        endpoint.pending_lookups().take(nonce);
        warn!(peer_addr = %addr, %error, "cannot send lookup");
        return Err(LookupError::Unreachable(addr));
    }

    let result = result_rx
        .await
        .map_err(|_| LookupError::Unreachable(addr))?;

    match result {
        LookupResult::Found { node, id } => {
            wire::resolve(endpoint, node, id).map_err(|error| match error {
                LocalRefError::MessageType => LookupError::TypeMismatch,
                LocalRefError::Unbound => LookupError::NotFound,
            })
        }

        LookupResult::NotFound => Err(LookupError::NotFound),

        LookupResult::TypeMismatch => Err(LookupError::TypeMismatch),
    }
}

/// A [Key] as it travels: the message type is a name rather than a type, since the wire has no
/// types. The tag is always derived from the type, never passed as a string, so a name and a tag
/// cannot be transposed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WireKey {
    pub(crate) name: String,
    pub(crate) type_tag: String,
}

impl WireKey {
    pub(crate) fn new<M>(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            type_tag: any::type_name::<M>().to_string(),
        }
    }
}

/// Answers are matched to their lookups by nonce rather than by key, so two lookups of one key
/// towards one node do not resolve each other's answer; pending replies are keyed the same way.
#[derive(Debug, Display, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct Nonce(u64);

impl Nonce {
    pub(crate) fn mint(next: &AtomicU64) -> Self {
        Self(next.fetch_add(1, Ordering::Relaxed))
    }

    #[cfg(test)]
    pub(crate) fn first() -> Self {
        Self(0)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum LookupResult {
    Found { node: NodeId, id: ActorId },
    NotFound,
    TypeMismatch,
}

/// The lookups awaiting an answer, which a reconnect re-sends and a node given up on fails: a
/// lookup lost with a connection would otherwise wait for an answer nobody owes anymore.
pub(crate) struct PendingLookups {
    next_nonce: AtomicU64,
    pending: Mutex<HashMap<Nonce, Pending>>,
}

impl PendingLookups {
    pub(crate) fn new() -> Self {
        Self {
            next_nonce: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn frames(&self, addr: SocketAddr) -> Vec<Frame<'static>> {
        lock(&self.pending)
            .iter()
            .filter(|(_, pending)| pending.addr == addr)
            .map(|(nonce, pending)| Frame::Lookup {
                nonce: *nonce,
                key: pending.key.clone(),
            })
            .collect()
    }

    /// Dropping the senders is what fails the lookups: their callers see the address as
    /// unreachable rather than waiting for a node this endpoint has given up on.
    pub(crate) fn fail(&self, addr: SocketAddr) {
        lock(&self.pending).retain(|_, pending| pending.addr != addr);
    }

    fn add(&self, addr: SocketAddr, key: WireKey) -> (Nonce, oneshot::Receiver<LookupResult>) {
        let nonce = Nonce::mint(&self.next_nonce);
        let (result_tx, result_rx) = oneshot::channel();

        lock(&self.pending).insert(
            nonce,
            Pending {
                addr,
                key,
                result_tx,
            },
        );
        (nonce, result_rx)
    }

    fn take(&self, nonce: Nonce) -> Option<Pending> {
        lock(&self.pending).remove(&nonce)
    }
}

pub(crate) fn on_lookup(endpoint: &EndpointInner, peer: NodeId, nonce: Nonce, key: WireKey) {
    let result = resolve_key(endpoint, &key);

    if let Err(error) = endpoint.send(peer, Frame::LookupReply { nonce, result }) {
        warn!(%peer, name = key.name.as_str(), %error, "cannot answer lookup");
    }
}

pub(crate) fn on_lookup_reply(endpoint: &EndpointInner, nonce: Nonce, result: LookupResult) {
    if let Some(pending) = endpoint.pending_lookups().take(nonce) {
        let _ = pending.result_tx.send(result);
    }
}

struct Pending {
    addr: SocketAddr,
    key: WireKey,
    result_tx: oneshot::Sender<LookupResult>,
}

fn resolve_key(endpoint: &EndpointInner, key: &WireKey) -> LookupResult {
    match endpoint.registry().named(key) {
        Named::Found(id) => LookupResult::Found {
            node: endpoint.node(),
            id,
        },

        Named::NotFound => LookupResult::NotFound,

        Named::TypeMismatch => LookupResult::TypeMismatch,
    }
}
