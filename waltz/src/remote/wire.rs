use crate::{
    ActorId, ActorRef,
    actor_ref::WatchTarget,
    remote::{
        codec::CodecError,
        endpoint::{self, EndpointInner},
        node::NodeId,
        registry::LocalRefError,
        sink::RemoteSink,
    },
};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, DeserializeOwned},
    ser,
};
use std::any;
use thiserror::Error;

/// A reference which cannot be serialized or resolved out of band.
#[derive(Debug, Error)]
pub enum RefError {
    /// The remoting endpoint has not been started, see [start](crate::remote::start).
    #[error("remoting endpoint not started")]
    EndpointNotStarted,

    /// The reference names another message type than the one it is resolved as. The comparison
    /// assumes both nodes are built from the same source, like the wire format does.
    #[error("reference of another message type")]
    TypeMismatch,

    /// The reference names an actor of this node which is not bound anymore, i.e. it has
    /// terminated.
    #[error("no actor bound under this reference")]
    Unbound,

    /// The reference bytes cannot be encoded or decoded.
    #[error(transparent)]
    Codec(#[from] CodecError),
}

impl From<LocalRefError> for RefError {
    fn from(error: LocalRefError) -> Self {
        match error {
            LocalRefError::Unbound => RefError::Unbound,
            LocalRefError::MessageType => RefError::TypeMismatch,
        }
    }
}

/// Serialize a reference for out of band exchange, e.g. via configuration or command line, using
/// the endpoint's codec. This is the bootstrap mechanism: any further reference travels inside a
/// message told to an already resolved one. The bytes carry the message type next to the
/// reference, so [deserialize_ref] can refuse a resolution as another type.
pub fn serialize_ref<M>(actor_ref: &ActorRef<M>) -> Result<Vec<u8>, RefError>
where
    M: DeserializeOwned + Send + 'static,
{
    let endpoint = endpoint::get().ok_or(RefError::EndpointNotStarted)?;
    let bytes = endpoint
        .codec()
        .encode(&(any::type_name::<M>(), actor_ref))?;
    Ok(bytes)
}

/// Resolve a reference serialized by [serialize_ref] on another node, using the endpoint's codec;
/// a reference serialized for another message type is refused as [RefError::TypeMismatch] rather
/// than resolved into a reference which drops every message told to it.
pub fn deserialize_ref<M>(bytes: &[u8]) -> Result<ActorRef<M>, RefError>
where
    M: Serialize + Send + 'static,
{
    let endpoint = endpoint::get().ok_or(RefError::EndpointNotStarted)?;

    let (type_tag, WireRef { node, id }) =
        endpoint.codec().decode_to::<(String, WireRef)>(bytes)?;

    if type_tag != any::type_name::<M>() {
        return Err(RefError::TypeMismatch);
    }

    let actor_ref = resolve(endpoint, node, id)?;
    Ok(actor_ref)
}

#[cfg_attr(docsrs, doc(cfg(feature = "remote")))]
impl<M> Serialize for ActorRef<M>
where
    M: DeserializeOwned + Send + 'static,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let Some(endpoint) = endpoint::get() else {
            return Err(ser::Error::custom(RefError::EndpointNotStarted));
        };

        let node = match self.watch_target() {
            WatchTarget::Remote(node) => node,

            WatchTarget::Local(_) => {
                endpoint.registry().bind(self);
                endpoint.node()
            }
        };

        WireRef {
            node,
            id: self.actor_id(),
        }
        .serialize(serializer)
    }
}

#[cfg_attr(docsrs, doc(cfg(feature = "remote")))]
impl<'de, M> Deserialize<'de> for ActorRef<M>
where
    M: Serialize + Send + 'static,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let WireRef { node, id } = WireRef::deserialize(deserializer)?;

        let Some(endpoint) = endpoint::get() else {
            return Err(de::Error::custom(RefError::EndpointNotStarted));
        };

        resolve(endpoint, node, id).map_err(|error| de::Error::custom(RefError::from(error)))
    }
}

/// A reference to an actor of this node resolves to the local one rather than to a remote sink
/// looping back through the endpoint, which is what makes a reference travelling a round trip
/// come home as the very reference it started as.
pub(crate) fn resolve<M>(
    endpoint: &EndpointInner,
    node: NodeId,
    id: ActorId,
) -> Result<ActorRef<M>, LocalRefError>
where
    M: Serialize + Send + 'static,
{
    if node == endpoint.node() {
        endpoint.registry().local_ref::<M>(id)
    } else {
        Ok(ActorRef::remote(RemoteSink::new(node, id)))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct WireRef {
    node: NodeId,
    id: ActorId,
}
