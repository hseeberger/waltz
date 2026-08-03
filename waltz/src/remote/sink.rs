use crate::{
    ActorId,
    remote::{
        codec::{Codec, CodecError},
        endpoint::{self, LaneError},
        frame::Frame,
        node::NodeId,
    },
};
use derive_more::Debug;
use serde::Serialize;
use thiserror::Error;

#[derive(Debug)]
pub(crate) struct RemoteSink<M> {
    node: NodeId,
    target: ActorId,
    #[debug(skip)]
    encode: fn(&M, &dyn Codec) -> Result<Vec<u8>, CodecError>,
}

impl<M> RemoteSink<M> {
    pub(crate) fn new(node: NodeId, target: ActorId) -> Self
    where
        M: Serialize,
    {
        Self {
            node,
            target,
            encode: encode_message::<M>,
        }
    }

    pub(crate) fn node(&self) -> NodeId {
        self.node
    }

    pub(crate) fn target(&self) -> ActorId {
        self.target
    }

    pub(crate) fn try_send_message(&self, message: M) -> Result<(), RemoteSendError> {
        let endpoint = endpoint::get().ok_or(RemoteSendError::EndpointNotStarted)?;
        let payload = (self.encode)(&message, endpoint.codec())?;
        endpoint.send(
            self.node,
            Frame::Message {
                target: self.target,
                payload,
            },
        )?;
        Ok(())
    }
}

impl<M> Clone for RemoteSink<M> {
    fn clone(&self) -> Self {
        Self {
            node: self.node,
            target: self.target,
            encode: self.encode,
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum RemoteSendError {
    #[error("remoting endpoint not started")]
    EndpointNotStarted,

    #[error(transparent)]
    Lane(#[from] LaneError),

    #[error(transparent)]
    Codec(#[from] CodecError),
}

fn encode_message<M>(message: &M, codec: &dyn Codec) -> Result<Vec<u8>, CodecError>
where
    M: Serialize,
{
    codec.encode(message)
}
