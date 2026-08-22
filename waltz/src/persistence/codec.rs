use serde::{Serialize, de::DeserializeOwned};
use std::error::Error;
use thiserror::Error;

/// Encodes events and snapshots into the payload bytes handed to the stores and decodes them
/// back. A codec must be self-describing enough that its output can be decoded without external
/// schema knowledge, and the codec reading a stream must be the one which wrote it.
pub trait Codec {
    /// Encode the given value into payload bytes.
    fn encode<T>(&self, value: &T) -> Result<Vec<u8>, EncodeError>
    where
        T: Serialize;

    /// Decode a value from the given payload bytes.
    fn decode<T>(&self, payload: &[u8]) -> Result<T, PayloadError>
    where
        T: DeserializeOwned;
}

/// The default codec: CBOR, self-describing and compact.
#[derive(Debug, Default, Clone, Copy)]
pub struct Cbor;

impl Codec for Cbor {
    fn encode<T>(&self, value: &T) -> Result<Vec<u8>, EncodeError>
    where
        T: Serialize,
    {
        let mut payload = Vec::new();
        ciborium::into_writer(value, &mut payload).map_err(EncodeError::new)?;

        Ok(payload)
    }

    fn decode<T>(&self, payload: &[u8]) -> Result<T, PayloadError>
    where
        T: DeserializeOwned,
    {
        ciborium::from_reader(payload).map_err(PayloadError::new)
    }
}

/// An alternative codec: JSON, directly readable in the stores at the cost of size and speed.
#[derive(Debug, Default, Clone, Copy)]
pub struct Json;

impl Codec for Json {
    fn encode<T>(&self, value: &T) -> Result<Vec<u8>, EncodeError>
    where
        T: Serialize,
    {
        serde_json::to_vec(value).map_err(EncodeError::new)
    }

    fn decode<T>(&self, payload: &[u8]) -> Result<T, PayloadError>
    where
        T: DeserializeOwned,
    {
        serde_json::from_slice(payload).map_err(PayloadError::new)
    }
}

/// The failure of [Codec::encode]; the codec's underlying error is its [source](Error::source).
#[derive(Debug, Error)]
#[error("value not encodable")]
pub struct EncodeError(#[source] Box<dyn Error + Send + Sync>);

impl EncodeError {
    /// Wrap a codec's underlying error.
    pub fn new<E>(error: E) -> Self
    where
        E: Into<Box<dyn Error + Send + Sync>>,
    {
        Self(error.into())
    }
}

/// The failure of [Codec::decode]; the codec's underlying error is its [source](Error::source).
/// An undecodable payload is the only failure a codec itself can produce. The manifest and
/// schema version checks around it fail with [DecodeError](crate::DecodeError) instead.
#[derive(Debug, Error)]
#[error("payload not decodable")]
pub struct PayloadError(#[source] Box<dyn Error + Send + Sync>);

impl PayloadError {
    /// Wrap a codec's underlying error.
    pub fn new<E>(error: E) -> Self
    where
        E: Into<Box<dyn Error + Send + Sync>>,
    {
        Self(error.into())
    }
}

#[cfg(test)]
mod tests {
    use crate::persistence::codec::{Cbor, Codec, Json};
    use serde::{Deserialize, Serialize};
    use std::error::Error;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Value {
        name: String,
        count: u64,
    }

    /// Both shipped codecs are self-describing as the trait promises: a payload decodes back into
    /// its own type with nothing but the bytes, no schema handed alongside.
    #[test]
    fn both_codecs_round_trip() {
        let value = value();

        let payload = Cbor.encode(&value).expect("the value is CBOR encodable");
        assert_eq!(
            Cbor.decode::<Value>(&payload)
                .expect("the payload is CBOR decodable"),
            value
        );

        let payload = Json.encode(&value).expect("the value is JSON encodable");
        assert_eq!(
            Json.decode::<Value>(&payload)
                .expect("the payload is JSON decodable"),
            value
        );
    }

    /// [Json] is documented as directly readable in the stores, which only holds if its output is
    /// text carrying the field names.
    #[test]
    fn json_encodes_readable_text() {
        let payload = Json.encode(&value()).expect("the value is JSON encodable");
        let text = String::from_utf8(payload).expect("JSON output is UTF-8");

        assert!(text.contains("\"name\""), "got {text}");
        assert!(text.contains("\"waltz\""), "got {text}");
    }

    /// The trait requires that the codec reading a stream is the one which wrote it; a payload
    /// from the other codec must fail rather than decode into a wrong value.
    #[test]
    fn a_payload_of_another_codec_does_not_decode() {
        let payload = Json.encode(&value()).expect("the value is JSON encodable");
        assert!(
            Cbor.decode::<Value>(&payload).is_err(),
            "CBOR must not decode a JSON payload"
        );

        let payload = Cbor.encode(&value()).expect("the value is CBOR encodable");
        assert!(
            Json.decode::<Value>(&payload).is_err(),
            "JSON must not decode a CBOR payload"
        );
    }

    /// A truncated payload is a decode failure, not a panic or a default value.
    #[test]
    fn an_empty_payload_does_not_decode() {
        assert!(Cbor.decode::<Value>(&[]).is_err());
        assert!(Json.decode::<Value>(&[]).is_err());
    }

    /// The codec's own error is reachable as the source, not merged into this error's message:
    /// a caller can walk the chain or downcast to the codec's concrete error type.
    #[test]
    fn the_codec_error_is_the_source() {
        let error = Cbor
            .decode::<Value>(&[])
            .expect_err("an empty payload must not decode");

        assert_eq!(error.to_string(), "payload not decodable");
        let source = error.source().expect("the codec error must be the source");
        assert_ne!(
            source.to_string(),
            error.to_string(),
            "the source must carry the codec's own text, not repeat this message"
        );
    }

    fn value() -> Value {
        Value {
            name: "waltz".to_string(),
            count: 42,
        }
    }
}
