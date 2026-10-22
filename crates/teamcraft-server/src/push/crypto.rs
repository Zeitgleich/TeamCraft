//! Typed adapters around `teamcraft_crypto::push`: all primitives live in that crate (security.md);
//! the server only turns [`PushWakeUp`] into bytes and back and draws randomness.

use rand_core::OsRng;
use teamcraft_crypto::push as crypto;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::PushWakeUp;

pub use teamcraft_crypto::push::{MAX_WEB_PUSH_PLAINTEXT, PushCryptoError, SubscriptionKeys, VapidKey};

/// Length of a push key (256 bits).
pub const PUSH_KEY_BYTES: usize = 32;

/// Seals a wake-up for a device with its push key.
pub fn seal_wakeup(push_key: &[u8], wake_up: &PushWakeUp) -> Result<Vec<u8>, PushCryptoError> {
  let key = SymmetricKey::from_slice(push_key).ok_or(PushCryptoError::BadPushKey)?;
  let plaintext = serde_json::to_vec(wake_up).map_err(|_| PushCryptoError::BadWakeUp)?;
  crypto::seal_wakeup(&key, &plaintext, &mut OsRng)
}

/// Opens a sealed wake-up. The apps do this; the server only needs it in tests.
pub fn open_wakeup(push_key: &[u8], sealed: &[u8]) -> Result<PushWakeUp, PushCryptoError> {
  let key = SymmetricKey::from_slice(push_key).ok_or(PushCryptoError::BadPushKey)?;
  let plaintext = crypto::open_wakeup(&key, sealed)?;
  serde_json::from_slice(plaintext.expose_secret()).map_err(|_| PushCryptoError::BadWakeUp)
}

/// Encrypts a payload for a browser subscription (RFC 8291).
pub fn encrypt_web_push(keys: &SubscriptionKeys, plaintext: &[u8]) -> Result<Vec<u8>, PushCryptoError> {
  crypto::encrypt_web_push(keys, plaintext, &mut OsRng)
}

/// Decrypts a Web Push body as the user agent would (tests).
#[cfg(feature = "integration")]
pub fn decrypt_web_push(
  ua_private: &crypto::UserAgentSecretKey,
  auth_secret: &[u8],
  body: &[u8],
) -> Result<Vec<u8>, PushCryptoError> {
  crypto::decrypt_web_push(ua_private, auth_secret, body).map(|bytes| bytes.expose_secret().to_vec())
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{ConversationId, Id, PushKind, Snowflake};

  use super::*;

  #[test]
  fn typed_wake_ups_round_trip_and_bad_keys_are_rejected() {
    let wake_up = PushWakeUp {
      kind: PushKind::Mention,
      conversation_id: ConversationId::from_snowflake(Snowflake::new(7).unwrap()),
      maybe_message_id: None,
      maybe_sender_id: None,
      is_urgent: true,
    };
    let key = [7u8; PUSH_KEY_BYTES];
    assert_eq!(
      open_wakeup(&key, &seal_wakeup(&key, &wake_up).unwrap()).unwrap(),
      wake_up
    );
    assert!(matches!(
      seal_wakeup(&[1u8; 5], &wake_up),
      Err(PushCryptoError::BadPushKey)
    ));
    assert!(open_wakeup(&[8u8; PUSH_KEY_BYTES], &seal_wakeup(&key, &wake_up).unwrap()).is_err());
  }
}
