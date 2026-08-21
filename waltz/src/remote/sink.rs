use crate::{
    ActorId, AskError,
    remote::{
        codec::{Codec, CodecError},
        endpoint::{self, LaneError},
        frame::Frame,
        node::NodeId,
        reply,
    },
};
use derive_more::Debug;
use serde::Serialize;
use std::borrow::Cow;
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

        let (payload, reply_tags) =
            reply::record_minted(|| (self.encode)(&message, endpoint.codec()));
        let sent = payload
            .map_err(RemoteSendError::from)
            .and_then(|payload| admit_payload(payload, endpoint.config().max_frame_size.get()))
            .and_then(|payload| {
                endpoint
                    .send(
                        self.node,
                        Frame::Message {
                            target: self.target,
                            reply_tags: reply_tags.clone(),
                            payload: Cow::Owned(payload),
                        },
                    )
                    .map_err(RemoteSendError::from)
            });

        match sent {
            Ok(()) => {
                endpoint.pending_replies().stamp(&reply_tags, self.node);
                Ok(())
            }

            Err(error) => {
                endpoint.pending_replies().discard(&reply_tags);
                Err(error)
            }
        }
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

    #[error("payload of {len} bytes exceeds the maximum frame size of {max} bytes")]
    PayloadTooLarge { len: usize, max: usize },
}

impl From<RemoteSendError> for AskError {
    fn from(error: RemoteSendError) -> Self {
        match error {
            RemoteSendError::Lane(LaneError::OutboundQueueFull(_)) => Self::MailboxFull,

            RemoteSendError::EndpointNotStarted
            | RemoteSendError::Lane(_)
            | RemoteSendError::Codec(_)
            | RemoteSendError::PayloadTooLarge { .. } => Self::ActorTerminated,
        }
    }
}

/// The frame adds a header on top, so a payload alone at the limit is already oversize; failing
/// here, before any reply nonce is stamped, is what keeps such an ask off its timeout. A smaller
/// payload whose framed size still exceeds the limit dies in the writer.
pub(crate) fn admit_payload(
    payload: Vec<u8>,
    max_frame_size: usize,
) -> Result<Vec<u8>, RemoteSendError> {
    if payload.len() >= max_frame_size {
        return Err(RemoteSendError::PayloadTooLarge {
            len: payload.len(),
            max: max_frame_size,
        });
    }

    Ok(payload)
}

fn encode_message<M>(message: &M, codec: &dyn Codec) -> Result<Vec<u8>, CodecError>
where
    M: Serialize,
{
    codec.encode(message)
}

#[cfg(test)]
mod tests {
    use crate::remote::sink::{RemoteSendError, admit_payload};

    /// A payload at or beyond the limit is refused at send time, since the frame header pushes it
    /// over anyway; one below it passes, even where the framed size might still exceed the limit,
    /// which the writer catches instead.
    #[test]
    fn admission_refuses_a_payload_reaching_the_limit() {
        assert!(admit_payload(vec![0; 31], 32).is_ok());

        assert!(matches!(
            admit_payload(vec![0; 32], 32),
            Err(RemoteSendError::PayloadTooLarge { len: 32, max: 32 })
        ));
        assert!(matches!(
            admit_payload(vec![0; 33], 32),
            Err(RemoteSendError::PayloadTooLarge { len: 33, max: 32 })
        ));
    }
}
