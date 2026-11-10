//! Typed adapters around `teamcraft_crypto::push`: all primitives live in that crate (security.md);
//! the server only turns [`PushWakeUp`] into bytes and back and draws randomness.

use rand_core::OsRng;
use teamcraft_crypto::ids::DeviceId as DeviceIdCrypto;
use teamcraft_crypto::push as crypto;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{DeviceId, Id, PushWakeUp};

pub use teamcraft_crypto::push::{MAX_WEB_PUSH_PLAINTEXT, PushCryptoError, SubscriptionKeys, VapidKey};

/// Length of a push key (256 bits).
pub const PUSH_KEY_BYTES: usize = 32;

/// The size of every visible wake-up once sealed (e2ee.md §7, "Message in the wake-up"). A push
/// gateway refuses larger ones (its `max_sealed_bytes`, 2048 by default), and base64url-encoded it
/// stays well below the 4 KB that APNs and FCM take, next to the alert or the data key.
pub const VISIBLE_WAKEUP_BYTES: usize = 2048;

/// Longest ciphertext that could fit a visible wake-up at all: base64 in JSON takes 4 bytes for 3.
/// Longer messages are never copied into wake-ups; shorter ones are checked when sealed.
pub const MAX_PUSHED_CIPHERTEXT_BYTES: usize = (VISIBLE_WAKEUP_BYTES - crypto::WAKEUP_OVERHEAD) / 4 * 3;

/// Seals a wake-up for a device with its push key. A visible one is padded to
/// [`VISIBLE_WAKEUP_BYTES`] and carries its message only if that still fits; without it the device
/// fetches the message. Silent wake-ups name nothing to show, carry no message and stay small.
pub fn seal_wakeup(push_key: &[u8], wake_up: &PushWakeUp) -> Result<Vec<u8>, PushCryptoError> {
  let key = SymmetricKey::from_slice(push_key).ok_or(PushCryptoError::BadPushKey)?;
  if !wake_up.kind.is_visible() {
    let plaintext = to_json(&wake_up.clone().without_message())?;
    return crypto::seal_wakeup(&key, &plaintext, &mut OsRng);
  }
  if wake_up.maybe_message.is_some() {
    match crypto::seal_wakeup_padded(&key, &to_json(wake_up)?, VISIBLE_WAKEUP_BYTES, &mut OsRng) {
      Err(PushCryptoError::TooLarge) => {},
      sealed => return sealed,
    }
  }
  let plaintext = to_json(&wake_up.clone().without_message())?;
  match crypto::seal_wakeup_padded(&key, &plaintext, VISIBLE_WAKEUP_BYTES, &mut OsRng) {
    // Invariant in practice: IDs, badge and times take a few hundred bytes. Should a future field
    // outgrow the size, the wake-up still goes out, only its length then tells it apart.
    Err(PushCryptoError::TooLarge) => crypto::seal_wakeup(&key, &plaintext, &mut OsRng),
    sealed => sealed,
  }
}

/// Seals a wake-up without its message and without padding, for a push service that refuses
/// [`VISIBLE_WAKEUP_BYTES`]: the device fetches the message, and the length shows what it names as
/// before the padding.
pub fn seal_wakeup_compact(push_key: &[u8], wake_up: &PushWakeUp) -> Result<Vec<u8>, PushCryptoError> {
  let key = SymmetricKey::from_slice(push_key).ok_or(PushCryptoError::BadPushKey)?;
  let plaintext = to_json(&wake_up.clone().without_message())?;
  crypto::seal_wakeup(&key, &plaintext, &mut OsRng)
}

fn to_json(wake_up: &PushWakeUp) -> Result<Vec<u8>, PushCryptoError> {
  serde_json::to_vec(wake_up).map_err(|_| PushCryptoError::BadWakeUp)
}

/// Opens a sealed wake-up. The apps do this; the server only needs it in tests.
pub fn open_wakeup(push_key: &[u8], sealed: &[u8]) -> Result<PushWakeUp, PushCryptoError> {
  let key = SymmetricKey::from_slice(push_key).ok_or(PushCryptoError::BadPushKey)?;
  let plaintext = crypto::open_wakeup(&key, sealed)?;
  serde_json::from_slice(plaintext.expose_secret()).map_err(|_| PushCryptoError::BadWakeUp)
}

/// Checks the service worker's proof for a rotated Web Push subscription (e2ee.md §7).
pub fn verify_subscription_change(
  push_key: &[u8],
  device: DeviceId,
  previous_token: &str,
  token: &str,
  mac: &[u8],
) -> Result<(), PushCryptoError> {
  let key = SymmetricKey::from_slice(push_key).ok_or(PushCryptoError::BadPushKey)?;
  // Invariant: snowflakes are never negative (`Snowflake::new` rejects them).
  let raw = u64::try_from(device.snowflake().get()).unwrap_or(0);
  crypto::verify_subscription_change(&key, DeviceIdCrypto::new(raw), previous_token, token, mac)
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
      maybe_thread_id: None,
      maybe_call_id: None,
      maybe_badge: None,
      maybe_sent_at: None,
      maybe_meeting: None,
      maybe_message: None,
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

  mod message_in_the_wake_up {
    use teamcraft_proto::{Base64Bytes, ClientMessageId, MessageId, PushedMessage, UserId};

    use super::*;

    const KEY: [u8; PUSH_KEY_BYTES] = [7; PUSH_KEY_BYTES];

    #[test]
    fn a_message_that_fits_travels_in_the_wake_up() {
      let wake_up = message_wake_up(Some(100));
      let sealed = seal_wakeup(&KEY, &wake_up).unwrap();
      assert_eq!(sealed.len(), VISIBLE_WAKEUP_BYTES);
      assert_eq!(open_wakeup(&KEY, &sealed).unwrap(), wake_up);
    }

    #[test]
    fn the_largest_message_that_fits_and_the_first_that_doesnt() {
      let fitting = largest_fitting_ciphertext();
      let sealed = seal_wakeup(&KEY, &message_wake_up(Some(fitting))).unwrap();
      assert_eq!(sealed.len(), VISIBLE_WAKEUP_BYTES);
      assert!(open_wakeup(&KEY, &sealed).unwrap().maybe_message.is_some());

      let sealed = seal_wakeup(&KEY, &message_wake_up(Some(fitting + 1))).unwrap();
      assert_eq!(sealed.len(), VISIBLE_WAKEUP_BYTES);
      let opened = open_wakeup(&KEY, &sealed).unwrap();
      assert_eq!(opened, message_wake_up(None));
    }

    #[test]
    fn every_visible_wake_up_has_the_same_size() {
      let sizes: Vec<usize> = [
        message_wake_up(None),
        message_wake_up(Some(1)),
        message_wake_up(Some(900)),
        message_wake_up(Some(MAX_PUSHED_CIPHERTEXT_BYTES)),
        message_wake_up(Some(60_000)),
        PushWakeUp {
          kind: PushKind::Call,
          ..message_wake_up(None)
        },
      ]
      .iter()
      .map(|wake_up| seal_wakeup(&KEY, wake_up).unwrap().len())
      .collect();
      assert!(
        sizes.iter().all(|size| *size == VISIBLE_WAKEUP_BYTES),
        "{sizes:?}"
      );
    }

    #[test]
    fn silent_wake_ups_never_carry_a_message_and_stay_small() {
      let dismiss = PushWakeUp {
        kind: PushKind::Dismiss,
        ..message_wake_up(Some(100))
      };
      let sealed = seal_wakeup(&KEY, &dismiss).unwrap();
      assert!(sealed.len() < 512);
      assert_eq!(open_wakeup(&KEY, &sealed).unwrap().maybe_message, None);
    }

    #[test]
    fn the_copy_limit_is_never_below_what_fits() {
      assert!(largest_fitting_ciphertext() <= MAX_PUSHED_CIPHERTEXT_BYTES);
    }

    /// Found by trying, so the test follows the JSON's real overhead.
    fn largest_fitting_ciphertext() -> usize {
      (1..=MAX_PUSHED_CIPHERTEXT_BYTES)
        .rev()
        .find(|len| {
          let sealed = seal_wakeup(&KEY, &message_wake_up(Some(*len))).unwrap();
          open_wakeup(&KEY, &sealed).unwrap().maybe_message.is_some()
        })
        .unwrap()
    }

    fn message_wake_up(maybe_ciphertext_len: Option<usize>) -> PushWakeUp {
      let id = |raw| Snowflake::new(raw).unwrap();
      PushWakeUp {
        maybe_badge: Some(12),
        maybe_sent_at: Some(teamcraft_proto::UnixMillis(1_700_000_000_000)),
        maybe_message: maybe_ciphertext_len.map(|len| PushedMessage {
          client_message_id: ClientMessageId::from_bytes([3; 16]),
          sender_device_id: DeviceId::from_snowflake(id(7_000_000_000_000_000_001)),
          content_key_id: Base64Bytes::new(vec![9; 16]),
          ciphertext: Base64Bytes::new(vec![0xA5; len]),
        }),
        ..PushWakeUp::for_message(
          PushKind::Mention,
          ConversationId::from_snowflake(id(7_000_000_000_000_000_002)),
          MessageId::from_snowflake(id(7_000_000_000_000_000_003)),
          UserId::from_snowflake(id(7_000_000_000_000_000_004)),
          true,
        )
      }
    }
  }
}
