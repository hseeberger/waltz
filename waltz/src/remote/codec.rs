use erased_serde::{Deserializer, Serialize};
use postcard::Deserializer as PostcardDeserializer;
use serde::de::DeserializeOwned;
use std::error::Error;
use thiserror::Error as ThisError;

/// The receiving side of [Codec::decode], applying a payload's deserializer to the concrete
/// message type.
///
/// The deserializer's object lifetime is independent of the payload lifetime `'de`, so a codec can
/// reclaim its concrete deserializer after the call, e.g. to check for leftover payload bytes.
pub type DecodeVisitor<'a> = &'a mut dyn for<'de, 'b> FnMut(
    &'b mut (dyn Deserializer<'de> + 'b),
) -> Result<(), erased_serde::Error>;

/// Encodes and decodes message payloads via serde; the default is [Postcard].
pub trait Codec
where
    Self: Send + Sync + 'static,
{
    /// Encode a message into payload bytes.
    fn encode(&self, message: &dyn Serialize) -> Result<Vec<u8>, CodecError>;

    /// Decode one message from the payload by applying `visit` to a deserializer over it: the
    /// deserializer borrows state living only for this call, hence it cannot be returned.
    fn decode(&self, payload: &[u8], visit: DecodeVisitor<'_>) -> Result<(), CodecError>;
}

impl dyn Codec {
    /// Decode one value of a concrete type, owning the visitor dance of [Codec::decode] once: the
    /// erased deserializer cannot return a value across the object boundary.
    pub(crate) fn decode_to<T>(&self, payload: &[u8]) -> Result<T, CodecError>
    where
        T: DeserializeOwned,
    {
        let mut decoded = None;
        self.decode(payload, &mut |deserializer| {
            decoded = Some(erased_serde::deserialize::<T>(deserializer)?);
            Ok(())
        })?;

        decoded.ok_or_else(|| CodecError::decoding("codec did not decode a value"))
    }
}

/// The postcard wire format, the default [Codec].
#[derive(Debug, Default, Clone, Copy)]
pub struct Postcard;

impl Codec for Postcard {
    fn encode(&self, message: &dyn Serialize) -> Result<Vec<u8>, CodecError> {
        postcard::to_stdvec(message).map_err(CodecError::encoding)
    }

    fn decode(&self, payload: &[u8], visit: DecodeVisitor<'_>) -> Result<(), CodecError> {
        let mut deserializer = PostcardDeserializer::from_bytes(payload);

        {
            let mut erased = <dyn Deserializer>::erase(&mut deserializer);
            visit(&mut erased).map_err(CodecError::decoding)?;
        }

        let rest = deserializer.finalize().map_err(CodecError::decoding)?;
        if rest.is_empty() {
            Ok(())
        } else {
            Err(CodecError::decoding("trailing payload bytes"))
        }
    }
}

/// A message payload which cannot be encoded or decoded, naming which of the two failed: the same
/// payload reaches a log site from both directions, so the error has to say which way it went.
#[derive(Debug, ThisError)]
pub enum CodecError {
    /// A message which cannot be encoded into payload bytes.
    #[error("cannot encode message")]
    Encode(#[source] Box<dyn Error + Send + Sync>),

    /// Payload bytes which cannot be decoded into a message.
    #[error("cannot decode message")]
    Decode(#[source] Box<dyn Error + Send + Sync>),
}

impl CodecError {
    /// Wrap any error as an encoding failure.
    pub fn encoding<E>(error: E) -> Self
    where
        E: Into<Box<dyn Error + Send + Sync>>,
    {
        Self::Encode(error.into())
    }

    /// Wrap any error as a decoding failure.
    pub fn decoding<E>(error: E) -> Self
    where
        E: Into<Box<dyn Error + Send + Sync>>,
    {
        Self::Decode(error.into())
    }
}

#[cfg(test)]
mod tests {
    use crate::remote::codec::{Codec, CodecError, Postcard};

    /// A round trip through the double erasure: the concrete postcard deserializer is erased into
    /// `dyn erased_serde::Deserializer` and only then applied to the concrete message type.
    #[test]
    fn a_message_survives_the_erased_round_trip() {
        let payload = Postcard.encode(&"hello".to_string()).expect("encodes");

        let mut decoded = None;
        Postcard
            .decode(&payload, &mut |deserializer| {
                decoded = Some(erased_serde::deserialize::<String>(deserializer)?);
                Ok(())
            })
            .expect("decodes");

        assert_eq!(decoded.as_deref(), Some("hello"));
    }

    /// Payload bytes which do not decode into the expected type are reported as a decoding
    /// failure, never as an encoding one.
    #[test]
    fn undecodable_payloads_name_the_direction() {
        let payload = Postcard.encode(&u64::MAX).expect("encodes");

        let error = Postcard
            .decode(&payload, &mut |deserializer| {
                erased_serde::deserialize::<bool>(deserializer)?;
                Ok(())
            })
            .expect_err("bool does not decode from a u64 payload");

        assert!(matches!(error, CodecError::Decode(_)));
    }

    /// A payload with bytes left after the message is corrupt or from a mismatched type and must
    /// be rejected: postcard deserializes a prefix and would otherwise accept it silently.
    #[test]
    fn trailing_payload_bytes_are_rejected() {
        let mut payload = Postcard.encode(&u64::MAX).expect("encodes");
        payload.push(0);

        let error = Postcard
            .decode(&payload, &mut |deserializer| {
                erased_serde::deserialize::<u64>(deserializer)?;
                Ok(())
            })
            .expect_err("a payload with trailing bytes does not decode");

        assert!(matches!(error, CodecError::Decode(_)));
    }
}
