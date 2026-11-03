//! Between the wire IDs of `teamcraft-proto` and the local ID types of `teamcraft-crypto`
//! (crypto README: those switch to proto IDs later; the u64 encoding stays the same).

use teamcraft_crypto::ids as crypto;
use teamcraft_proto::{ClientMessageId, ConversationId, DeviceId, Id, Snowflake, UserId};

use crate::error::CoreError;

pub(crate) fn user(id: UserId) -> crypto::UserId {
  crypto::UserId::new(raw(id))
}

pub(crate) fn device(id: DeviceId) -> crypto::DeviceId {
  crypto::DeviceId::new(raw(id))
}

pub(crate) fn conversation(id: ConversationId) -> crypto::ConversationId {
  crypto::ConversationId::new(raw(id))
}

pub(crate) fn client_message(id: ClientMessageId) -> crypto::ClientMessageId {
  crypto::ClientMessageId::from_bytes(*id.as_bytes())
}

pub(crate) fn user_from_crypto(id: crypto::UserId) -> Result<UserId, CoreError> {
  from_raw(id.get())
}

pub(crate) fn device_from_crypto(id: crypto::DeviceId) -> Result<DeviceId, CoreError> {
  from_raw(id.get())
}

pub(crate) fn conversation_from_crypto(id: crypto::ConversationId) -> Result<ConversationId, CoreError> {
  from_raw(id.get())
}

fn raw(id: impl Id) -> u64 {
  // Invariant: snowflakes are never negative (`Snowflake::new` rejects them).
  u64::try_from(id.snowflake().get()).unwrap_or(0)
}

fn from_raw<T: Id>(raw: u64) -> Result<T, CoreError> {
  let value = i64::try_from(raw).map_err(|_| CoreError::Corrupt(format!("ID {raw} out of range")))?;
  Snowflake::new(value)
    .map(T::from_snowflake)
    .map_err(|error| CoreError::Corrupt(error.to_string()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ids_round_trip() {
    let user_id = UserId::from_snowflake(Snowflake::new(123_456_789).unwrap());
    assert_eq!(user_from_crypto(user(user_id)).unwrap(), user_id);
    let device_id = DeviceId::from_snowflake(Snowflake::new(42).unwrap());
    assert_eq!(device_from_crypto(device(device_id)).unwrap(), device_id);
    assert!(user_from_crypto(crypto::UserId::new(u64::MAX)).is_err());
  }
}
