use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Opaque bytes (keys, ciphertext) that travel as standard base64 in JSON.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Base64Bytes(Vec<u8>);

impl Base64Bytes {
  pub fn new(bytes: Vec<u8>) -> Self {
    Self(bytes)
  }

  pub fn as_slice(&self) -> &[u8] {
    &self.0
  }

  pub fn into_vec(self) -> Vec<u8> {
    self.0
  }

  pub fn len(&self) -> usize {
    self.0.len()
  }

  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
}

// Ciphertext and keys don't belong in logs, not even as bytes.
impl fmt::Debug for Base64Bytes {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "Base64Bytes({} bytes)", self.0.len())
  }
}

impl From<Vec<u8>> for Base64Bytes {
  fn from(bytes: Vec<u8>) -> Self {
    Self(bytes)
  }
}

impl Serialize for Base64Bytes {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&STANDARD.encode(&self.0))
  }
}

impl<'de> Deserialize<'de> for Base64Bytes {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    struct BytesVisitor;

    impl Visitor<'_> for BytesVisitor {
      type Value = Base64Bytes;

      fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a standard base64 string")
      }

      fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        STANDARD.decode(v).map(Base64Bytes).map_err(E::custom)
      }
    }

    deserializer.deserialize_str(BytesVisitor)
  }
}

impl utoipa::PartialSchema for Base64Bytes {
  fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    utoipa::openapi::schema::ObjectBuilder::new()
      .schema_type(utoipa::openapi::schema::SchemaType::Type(
        utoipa::openapi::schema::Type::String,
      ))
      .format(Some(utoipa::openapi::SchemaFormat::KnownFormat(
        utoipa::openapi::KnownFormat::Byte,
      )))
      .into()
  }
}

impl utoipa::ToSchema for Base64Bytes {}

/// Client-generated 128-bit message ID. It names the message before the server assigns the
/// ordering Snowflake and makes sends idempotent. Wire format: 32 lowercase hex characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientMessageId([u8; 16]);

/// Client-generated key of a create request (space, channel, meeting). The server remembers it per
/// user and returns the original result for a repeated request. Same format as [`ClientMessageId`].
pub type IdempotencyKey = ClientMessageId;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("client message ID must be 32 hex characters")]
pub struct ClientMessageIdError;

impl ClientMessageId {
  /// A fresh random ID from the operating system's random source.
  pub fn random() -> Self {
    let mut bytes = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut bytes);
    Self(bytes)
  }

  pub fn from_bytes(bytes: [u8; 16]) -> Self {
    Self(bytes)
  }

  pub fn as_bytes(&self) -> &[u8; 16] {
    &self.0
  }

  pub fn to_hex(&self) -> String {
    self.0.iter().map(|byte| format!("{byte:02x}")).collect()
  }

  pub fn from_hex(text: &str) -> Result<Self, ClientMessageIdError> {
    if text.len() != 32 || !text.is_ascii() {
      return Err(ClientMessageIdError);
    }
    let mut bytes = [0u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
      *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).map_err(|_| ClientMessageIdError)?;
    }
    Ok(Self(bytes))
  }
}

impl TryFrom<&[u8]> for ClientMessageId {
  type Error = ClientMessageIdError;

  fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
    <[u8; 16]>::try_from(bytes)
      .map(Self)
      .map_err(|_| ClientMessageIdError)
  }
}

impl fmt::Display for ClientMessageId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.to_hex())
  }
}

impl Serialize for ClientMessageId {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&self.to_hex())
  }
}

impl<'de> Deserialize<'de> for ClientMessageId {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let text = String::deserialize(deserializer)?;
    Self::from_hex(&text).map_err(de::Error::custom)
  }
}

impl utoipa::PartialSchema for ClientMessageId {
  fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    utoipa::openapi::schema::ObjectBuilder::new()
      .schema_type(utoipa::openapi::schema::SchemaType::Type(
        utoipa::openapi::schema::Type::String,
      ))
      .pattern(Some("^[0-9a-f]{32}$"))
      .into()
  }
}

impl utoipa::ToSchema for ClientMessageId {}

/// A point in time as Unix milliseconds (a JSON number, safe below 2^53).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixMillis(pub i64);

impl utoipa::PartialSchema for UnixMillis {
  fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    utoipa::openapi::schema::ObjectBuilder::new()
      .schema_type(utoipa::openapi::schema::SchemaType::Type(
        utoipa::openapi::schema::Type::Integer,
      ))
      .format(Some(utoipa::openapi::SchemaFormat::KnownFormat(
        utoipa::openapi::KnownFormat::Int64,
      )))
      .description(Some("Unix time in milliseconds"))
      .into()
  }
}

impl utoipa::ToSchema for UnixMillis {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn base64_round_trips_and_hides_content_in_debug() {
    let bytes = Base64Bytes::new(vec![0, 1, 2, 250]);
    let json = serde_json::to_string(&bytes).unwrap();
    assert_eq!(json, "\"AAEC+g==\"");
    assert_eq!(serde_json::from_str::<Base64Bytes>(&json).unwrap(), bytes);
    assert_eq!(format!("{bytes:?}"), "Base64Bytes(4 bytes)");
  }

  #[test]
  fn base64_rejects_garbage() {
    assert!(serde_json::from_str::<Base64Bytes>("\"not base64!\"").is_err());
    assert!(serde_json::from_str::<Base64Bytes>("5").is_err());
  }

  #[test]
  fn client_message_id_round_trips() {
    let id = ClientMessageId::from_bytes([0xab; 16]);
    assert_eq!(
      serde_json::to_string(&id).unwrap(),
      format!("\"{}\"", "ab".repeat(16))
    );
    assert_eq!(
      serde_json::from_str::<ClientMessageId>(&serde_json::to_string(&id).unwrap()).unwrap(),
      id
    );
    assert_eq!(ClientMessageId::try_from(&[0xab_u8; 16][..]).unwrap(), id);
  }

  #[test]
  fn client_message_id_rejects_wrong_input() {
    for bad in [
      "".to_owned(),
      "ab".to_owned(),
      "zz".repeat(16),
      "ab".repeat(17),
      "é".repeat(16),
    ] {
      assert!(ClientMessageId::from_hex(&bad).is_err(), "{bad:?}");
    }
    assert!(ClientMessageId::try_from(&[0u8; 15][..]).is_err());
  }
}
