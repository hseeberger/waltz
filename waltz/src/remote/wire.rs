use crate::{
    ActorId, ActorRef,
    actor_ref::WatchTarget,
    remote::{codec::CodecError, endpoint, node::NodeId, sink::RemoteSink},
};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, DeserializeOwned},
    ser,
};
use thiserror::Error;

/// A reference which cannot be serialized or resolved out of band.
#[derive(Debug, Error)]
pub enum RefError {
    /// The remoting endpoint has not been started, see [start](crate::remote::start).
    #[error("remoting endpoint not started")]
    EndpointNotStarted,

    /// The reference bytes cannot be encoded or decoded.
    #[error(transparent)]
    Codec(#[from] CodecError),
}

/// Serialize a reference for out of band exchange, e.g. via configuration or command line, using
/// the endpoint's codec. This is the bootstrap mechanism: any further reference travels inside a
/// message told to an already resolved one.
pub fn serialize_ref<M>(actor_ref: &ActorRef<M>) -> Result<Vec<u8>, RefError>
where
    M: DeserializeOwned + Send + 'static,
{
    let endpoint = endpoint::get().ok_or(RefError::EndpointNotStarted)?;
    let bytes = endpoint.codec().encode(actor_ref)?;
    Ok(bytes)
}

/// Resolve a reference serialized by [serialize_ref] on another node, using the endpoint's codec.
pub fn deserialize_ref<M>(bytes: &[u8]) -> Result<ActorRef<M>, RefError>
where
    M: Serialize + Send + 'static,
{
    let endpoint = endpoint::get().ok_or(RefError::EndpointNotStarted)?;

    let mut actor_ref = None;
    endpoint.codec().decode(bytes, &mut |deserializer| {
        actor_ref = Some(erased_serde::deserialize::<ActorRef<M>>(deserializer)?);
        Ok(())
    })?;
    actor_ref
        .ok_or_else(|| CodecError::decoding("codec did not decode a reference".to_string()).into())
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

        if node == endpoint.node() {
            endpoint
                .registry()
                .local_ref::<M>(id)
                .map_err(de::Error::custom)
        } else {
            Ok(ActorRef::remote(RemoteSink::new(node, id)))
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct WireRef {
    node: NodeId,
    id: ActorId,
}
