use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Custom epoch of the Snowflake IDs: 2026-01-01T00:00:00Z in Unix milliseconds.
pub const SNOWFLAKE_EPOCH_MS: i64 = 1_767_225_600_000;
/// Bits of the per-millisecond sequence number (lowest bits).
pub const SNOWFLAKE_SEQUENCE_BITS: u32 = 12;
/// Bits of the worker (server instance) number, above the sequence.
pub const SNOWFLAKE_WORKER_BITS: u32 = 10;

/// A non-negative 63-bit time-sortable ID, assigned by the server.
///
/// Layout: 41 bits milliseconds since [`SNOWFLAKE_EPOCH_MS`], 10 bits worker, 12 bits sequence. Fits
/// a Postgres `BIGINT`. On the wire it is a decimal string because JavaScript numbers lose
/// precision above 2^53.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Snowflake(i64);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
  #[error("ID must not be negative: {0}")]
  Negative(i64),
  #[error("invalid ID: {0:?}")]
  Malformed(String),
}

impl Snowflake {
  /// The smallest ID, older than anything the server generates.
  pub const ZERO: Snowflake = Snowflake(0);

  pub fn new(raw: i64) -> Result<Self, IdError> {
    if raw < 0 {
      return Err(IdError::Negative(raw));
    }
    Ok(Self(raw))
  }

  /// The raw value, usable as Postgres `BIGINT`.
  pub fn get(self) -> i64 {
    self.0
  }

  /// Creation time in Unix milliseconds.
  pub fn unix_millis(self) -> i64 {
    (self.0 >> (SNOWFLAKE_SEQUENCE_BITS + SNOWFLAKE_WORKER_BITS)) + SNOWFLAKE_EPOCH_MS
  }

  pub fn worker(self) -> u16 {
    ((self.0 >> SNOWFLAKE_SEQUENCE_BITS) & ((1 << SNOWFLAKE_WORKER_BITS) - 1)) as u16
  }

  pub fn sequence(self) -> u16 {
    (self.0 & ((1 << SNOWFLAKE_SEQUENCE_BITS) - 1)) as u16
  }
}

impl fmt::Display for Snowflake {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.0)
  }
}

impl FromStr for Snowflake {
  type Err = IdError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let raw: i64 = s.parse().map_err(|_| IdError::Malformed(s.to_owned()))?;
    Self::new(raw)
  }
}

impl Serialize for Snowflake {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(self)
  }
}

impl<'de> Deserialize<'de> for Snowflake {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    struct SnowflakeVisitor;

    impl Visitor<'_> for SnowflakeVisitor {
      type Value = Snowflake;

      fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a decimal string holding a non-negative 63-bit ID")
      }

      fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        v.parse().map_err(E::custom)
      }
    }

    deserializer.deserialize_str(SnowflakeVisitor)
  }
}

impl utoipa::PartialSchema for Snowflake {
  fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    utoipa::openapi::schema::ObjectBuilder::new()
      .schema_type(utoipa::openapi::schema::SchemaType::Type(
        utoipa::openapi::schema::Type::String,
      ))
      .pattern(Some("^[0-9]{1,19}$"))
      .description(Some("Snowflake ID as decimal string"))
      .into()
  }
}

impl utoipa::ToSchema for Snowflake {}

/// Common behaviour of all typed Snowflake IDs.
pub trait Id: Copy {
  fn from_snowflake(snowflake: Snowflake) -> Self;
  fn snowflake(self) -> Snowflake;
}

/// Defines a typed Snowflake ID newtype with the full trait set.
macro_rules! define_id {
  ($(#[$meta:meta])* $name:ident) => {
    $(#[$meta])*
    #[derive(
      Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, ::serde::Serialize, ::serde::Deserialize,
    )]
    #[serde(transparent)]
    pub struct $name($crate::Snowflake);

    impl $crate::Id for $name {
      fn from_snowflake(snowflake: $crate::Snowflake) -> Self {
        Self(snowflake)
      }

      fn snowflake(self) -> $crate::Snowflake {
        self.0
      }
    }

    impl ::std::fmt::Display for $name {
      fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        ::std::fmt::Display::fmt(&self.0, f)
      }
    }

    impl ::std::str::FromStr for $name {
      type Err = $crate::IdError;

      fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
      }
    }

    impl ::utoipa::PartialSchema for $name {
      fn schema() -> ::utoipa::openapi::RefOr<::utoipa::openapi::schema::Schema> {
        <$crate::Snowflake as ::utoipa::PartialSchema>::schema()
      }
    }

    impl ::utoipa::ToSchema for $name {}
  };
}

define_id!(
  /// A user account.
  UserId
);
define_id!(
  /// A device (one set of keys) of a user.
  DeviceId
);
define_id!(
  /// A space (workspace/team).
  SpaceId
);
define_id!(
  /// A DM, group or channel.
  ConversationId
);
define_id!(
  /// A message; ordering by ID is the order within a conversation.
  MessageId
);
define_id!(
  /// An admin-defined category of channels inside a space.
  CategoryId
);
define_id!(
  /// A pending invitation.
  InviteId
);
define_id!(
  /// An audit log entry; IDs increase with time.
  AuditEntryId
);
define_id!(
  /// An uploaded encrypted file.
  AttachmentId
);
define_id!(
  /// An entry of a conversation's MLS delivery queue; IDs increase in delivery order.
  MlsMessageId
);
define_id!(
  /// A thread, identified by the ID of its root message.
  ThreadId
);
define_id!(
  /// A call (one SFU room, one DAVE group).
  CallId
);

#[cfg(test)]
mod tests {
  use super::*;

  const SAMPLE: i64 = 9_007_199_254_740_993; // 2^53 + 1, not representable as JSON number in JS

  #[test]
  fn serializes_as_decimal_string() {
    let id = UserId::from_snowflake(Snowflake::new(SAMPLE).unwrap());
    assert_eq!(serde_json::to_string(&id).unwrap(), "\"9007199254740993\"");
  }

  #[test]
  fn round_trips_through_json() {
    let id = MessageId::from_snowflake(Snowflake::new(SAMPLE).unwrap());
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(serde_json::from_str::<MessageId>(&json).unwrap(), id);
  }

  #[test]
  fn rejects_numbers_negatives_and_garbage() {
    assert!(serde_json::from_str::<UserId>("123").is_err());
    assert!(serde_json::from_str::<UserId>("\"-1\"").is_err());
    assert!(serde_json::from_str::<UserId>("\"abc\"").is_err());
    assert!(serde_json::from_str::<UserId>("\"\"").is_err());
    assert!(serde_json::from_str::<UserId>("\"9223372036854775808\"").is_err());
  }

  #[test]
  fn ids_of_different_types_are_distinct_types_with_same_ordering() {
    let a = ThreadId::from_snowflake(Snowflake::new(1).unwrap());
    let b = ThreadId::from_snowflake(Snowflake::new(2).unwrap());
    assert!(a < b);
  }

  #[test]
  fn decodes_snowflake_parts() {
    let raw = (5_000_i64 << 22) | (7_i64 << 12) | 42;
    let flake = Snowflake::new(raw).unwrap();
    assert_eq!(flake.unix_millis(), SNOWFLAKE_EPOCH_MS + 5_000);
    assert_eq!(flake.worker(), 7);
    assert_eq!(flake.sequence(), 42);
  }

  #[test]
  fn parses_from_str() {
    assert_eq!("42".parse::<SpaceId>().unwrap().snowflake().get(), 42);
    assert_eq!(Snowflake::new(-5), Err(IdError::Negative(-5)));
  }
}
