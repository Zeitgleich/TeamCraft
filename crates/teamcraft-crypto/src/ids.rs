//! Minimal ID newtypes used inside signed and encrypted structures.
//!
//! TODO(proto): switch to the shared ID types from `teamcraft-proto` once this crate may depend on
//! it. These are Snowflake IDs (`u64`), matching the server's primary keys; the binary encoding
//! (big-endian `u64`) is part of every signature and must stay stable when switching.

use std::fmt;

use tls_codec::{TlsDeserializeBytes, TlsSerialize, TlsSize};

macro_rules! snowflake_id {
  ($(#[$meta:meta])* $name:ident) => {
    $(#[$meta])*
    #[derive(
      Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, TlsSerialize, TlsDeserializeBytes, TlsSize,
    )]
    pub struct $name(u64);

    impl $name {
      pub const fn new(value: u64) -> Self {
        Self(value)
      }

      pub const fn get(self) -> u64 {
        self.0
      }
    }

    impl fmt::Display for $name {
      fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
      }
    }
  };
}

snowflake_id!(
  /// A user account.
  UserId
);
snowflake_id!(
  /// One device (client installation) of a user. Globally unique, not per user.
  DeviceId
);
snowflake_id!(
  /// A conversation: channel, group DM or DM. One MLS group per conversation.
  ConversationId
);
snowflake_id!(
  /// A message. It is bound into the message encryption, so the sender must know it before
  /// encrypting (see the crate README, open questions).
  MessageId
);
