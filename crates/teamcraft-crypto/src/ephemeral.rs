//! Ephemeral signals sealed with a conversation's content key (e2ee.md §4.11, "Who has the page
//! open"): small notes a device sends to the members connected right now and the server relays
//! without keeping them, such as who has a page of a channel's notes open and where their caret is.
//!
//! Sealed like a message (§4.4): encrypted with the conversation's current content key and signed
//! by the sending device, bound to conversation, sender user and device, so the server can't
//! move a signal to another conversation or attribute it to somebody else, and another member
//! can't speak in someone else's name. The labels of the associated data and the signature are
//! their own ([`domain::EPHEMERAL_AAD`], [`domain::EPHEMERAL_SIGNATURE`]): a signal never opens
//! as a message, nor a message as a signal. Signals carry no message ID; ordering and replay
//! are the receiver's business (a counter inside the plaintext).

use rand_core::CryptoRngCore;
use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize};

use crate::aead::{self, NONCE_LEN};
use crate::content_key::{ContentKeyId, ContentKeyRing};
use crate::domain;
use crate::identity::{DeviceKeyPair, DeviceSigningPublicKey, Ed25519Signature};
use crate::ids::{ConversationId, DeviceId, UserId};
use crate::secret::SecretBytes;

const SEALED_SIGNAL_VERSION: u8 = 1;

/// Who sent a signal into which conversation: taken from the relay's envelope (the server names
/// the sender's user and device), never from inside the ciphertext.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SignalContext {
  pub conversation_id: ConversationId,
  pub sender_user_id: UserId,
  pub sender_device_id: DeviceId,
}

/// A sealed signal as it travels.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SealedSignal {
  version: u8,
  content_key_id: ContentKeyId,
  nonce: [u8; NONCE_LEN],
  ciphertext: Vec<u8>,
  signature: Ed25519Signature,
}

#[derive(TlsSerialize, TlsSize)]
struct SignalAad {
  version: u8,
  context: SignalContext,
  content_key_id: ContentKeyId,
}

#[derive(Debug, thiserror::Error)]
pub enum SignalError {
  #[error("conversation has no current content key")]
  NoCurrentKey,
  #[error("content key {0:?} is not available on this device")]
  UnknownKey(ContentKeyId),
  #[error("key ring belongs to conversation {ring} but the signal to conversation {signal}")]
  WrongConversation {
    ring: ConversationId,
    signal: ConversationId,
  },
  #[error("unsupported sealed signal version {0}")]
  UnsupportedVersion(u8),
  #[error("the signing device isn't the signal's sender")]
  SignerMismatch,
  #[error("the signal isn't signed by its sender's device")]
  InvalidSignature,
  #[error("encryption failed")]
  EncryptionFailed,
  #[error("signal could not be decrypted (wrong key, wrong sender or tampered)")]
  DecryptionFailed,
  #[error("encoding failed: {0}")]
  Encoding(#[from] tls_codec::Error),
}

/// Seals a signal with the conversation's current content key and signs it with the sending
/// device's key (`signer` must be the device in `context`).
pub fn seal_signal(
  ring: &ContentKeyRing,
  context: &SignalContext,
  plaintext: &[u8],
  signer: &DeviceKeyPair,
  rng: &mut impl CryptoRngCore,
) -> Result<SealedSignal, SignalError> {
  ensure_conversation(ring, context)?;
  if signer.user_id() != context.sender_user_id || signer.device_id() != context.sender_device_id {
    return Err(SignalError::SignerMismatch);
  }
  let key = ring.current().ok_or(SignalError::NoCurrentKey)?;
  let aad = signal_aad(SEALED_SIGNAL_VERSION, context, key.id())?;
  let (nonce, ciphertext) =
    aead::seal(key.key(), &aad, plaintext, rng).map_err(|_| SignalError::EncryptionFailed)?;
  let signature = signer.sign_labeled(
    domain::EPHEMERAL_SIGNATURE,
    &signing_input(&aad, &nonce, &ciphertext),
  );
  Ok(SealedSignal {
    version: SEALED_SIGNAL_VERSION,
    content_key_id: key.id(),
    nonce,
    ciphertext,
    signature,
  })
}

/// Opens a signal: `context` from the relay's envelope, `sender` the signing key the sender
/// device's verified certificate names. Any content key of the ring opens it, so a signal sealed
/// just before a rotation still reads.
pub fn open_signal(
  ring: &ContentKeyRing,
  context: &SignalContext,
  signal: &SealedSignal,
  sender: &DeviceSigningPublicKey,
) -> Result<SecretBytes, SignalError> {
  ensure_conversation(ring, context)?;
  if signal.version != SEALED_SIGNAL_VERSION {
    return Err(SignalError::UnsupportedVersion(signal.version));
  }
  let key = ring
    .get(signal.content_key_id)
    .ok_or(SignalError::UnknownKey(signal.content_key_id))?;
  let aad = signal_aad(signal.version, context, key.id())?;
  sender
    .verify_labeled(
      domain::EPHEMERAL_SIGNATURE,
      &signing_input(&aad, &signal.nonce, &signal.ciphertext),
      &signal.signature,
    )
    .map_err(|_| SignalError::InvalidSignature)?;
  aead::open(key.key(), &signal.nonce, &aad, &signal.ciphertext).map_err(|_| SignalError::DecryptionFailed)
}

impl SealedSignal {
  pub fn to_bytes(&self) -> Result<Vec<u8>, SignalError> {
    Ok(self.tls_serialize_detached()?)
  }

  pub fn from_bytes(bytes: &[u8]) -> Result<Self, SignalError> {
    Ok(Self::tls_deserialize_exact_bytes(bytes)?)
  }
}

fn ensure_conversation(ring: &ContentKeyRing, context: &SignalContext) -> Result<(), SignalError> {
  if ring.conversation_id() == context.conversation_id {
    Ok(())
  } else {
    Err(SignalError::WrongConversation {
      ring: ring.conversation_id(),
      signal: context.conversation_id,
    })
  }
}

fn signal_aad(
  version: u8,
  context: &SignalContext,
  content_key_id: ContentKeyId,
) -> Result<Vec<u8>, SignalError> {
  let aad = SignalAad {
    version,
    context: *context,
    content_key_id,
  };
  Ok(domain::labeled(
    domain::EPHEMERAL_AAD,
    &aad.tls_serialize_detached()?,
  ))
}

fn signing_input(aad: &[u8], nonce: &[u8; NONCE_LEN], ciphertext: &[u8]) -> Vec<u8> {
  let mut input = Vec::with_capacity(aad.len() + 8 + NONCE_LEN + ciphertext.len());
  input.extend_from_slice(&(aad.len() as u64).to_be_bytes());
  input.extend_from_slice(aad);
  input.extend_from_slice(nonce);
  input.extend_from_slice(ciphertext);
  input
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;
  use crate::ids::ClientMessageId;
  use crate::message::{EncryptedMessage, MessageContext, decrypt_message, encrypt_message};

  const CONVERSATION: ConversationId = ConversationId::new(100);
  const PLAINTEXT: &[u8] = br#"{"seq":1,"state":"open"}"#;

  #[test]
  fn round_trip_through_bytes() {
    let ring = ring_with_key();
    let sealed = seal_signal(&ring, &context(), PLAINTEXT, &sender(), &mut OsRng).unwrap();
    let decoded = SealedSignal::from_bytes(&sealed.to_bytes().unwrap()).unwrap();
    let plaintext = open_signal(&ring, &context(), &decoded, &sender_key()).unwrap();
    assert_eq!(plaintext.expose_secret(), PLAINTEXT);
  }

  #[test]
  fn a_signal_sealed_before_a_rotation_still_opens() {
    let mut ring = ring_with_key();
    let sealed = seal_signal(&ring, &context(), PLAINTEXT, &sender(), &mut OsRng).unwrap();
    ring.rotate(&mut OsRng);
    assert!(open_signal(&ring, &context(), &sealed, &sender_key()).is_ok());
  }

  mod rejection {
    use super::*;

    #[test]
    fn rejects_tampering() {
      let ring = ring_with_key();
      let sealed = seal_signal(&ring, &context(), PLAINTEXT, &sender(), &mut OsRng).unwrap();
      let mut flipped_body = sealed.clone();
      flipped_body.ciphertext[0] ^= 1;
      let mut flipped_nonce = sealed.clone();
      flipped_nonce.nonce[0] ^= 1;
      let mut truncated = sealed;
      truncated.ciphertext.truncate(3);
      for tampered in [flipped_body, flipped_nonce, truncated] {
        assert!(matches!(
          open_signal(&ring, &context(), &tampered, &sender_key()),
          Err(SignalError::InvalidSignature)
        ));
      }
    }

    #[test]
    fn rejects_a_signal_moved_or_reattributed() {
      let ring = ring_with_key();
      let sealed = seal_signal(&ring, &context(), PLAINTEXT, &sender(), &mut OsRng).unwrap();
      let other_user = SignalContext {
        sender_user_id: UserId::new(99),
        ..context()
      };
      let other_device = SignalContext {
        sender_device_id: DeviceId::new(99),
        ..context()
      };
      for wrong in [other_user, other_device] {
        assert!(matches!(
          open_signal(&ring, &wrong, &sealed, &sender_key()),
          Err(SignalError::InvalidSignature)
        ));
      }
      let other_ring = {
        let mut ring = ContentKeyRing::new(ConversationId::new(101));
        ring.rotate(&mut OsRng);
        ring
      };
      let moved = SignalContext {
        conversation_id: ConversationId::new(101),
        ..context()
      };
      assert!(open_signal(&other_ring, &moved, &sealed, &sender_key()).is_err());
    }

    #[test]
    fn another_member_cant_sign_in_someone_elses_name() {
      let ring = ring_with_key();
      let impostor = DeviceKeyPair::from_secrets(
        UserId::new(2),
        DeviceId::new(20),
        &crate::secret::SymmetricKey::from_bytes([5; 32]),
        &crate::secret::SymmetricKey::from_bytes([6; 32]),
      );
      assert!(matches!(
        seal_signal(&ring, &context(), PLAINTEXT, &impostor, &mut OsRng),
        Err(SignalError::SignerMismatch)
      ));
      let own = SignalContext {
        sender_user_id: UserId::new(2),
        sender_device_id: DeviceId::new(20),
        ..context()
      };
      let sealed = seal_signal(&ring, &own, PLAINTEXT, &impostor, &mut OsRng).unwrap();
      // Relabeled as the victim's: the victim's key doesn't verify it.
      assert!(open_signal(&ring, &context(), &sealed, &sender_key()).is_err());
    }

    #[test]
    fn a_signal_never_opens_as_a_message_nor_a_message_as_a_signal() {
      let ring = ring_with_key();
      let sealed = seal_signal(&ring, &context(), PLAINTEXT, &sender(), &mut OsRng).unwrap();
      let message_context = MessageContext {
        conversation_id: CONVERSATION,
        sender_user_id: UserId::new(1),
        sender_device_id: DeviceId::new(10),
        client_message_id: ClientMessageId::from_bytes([1; 16]),
      };
      if let Ok(as_message) = EncryptedMessage::from_bytes(&sealed.to_bytes().unwrap()) {
        assert!(decrypt_message(&ring, &message_context, &as_message, &sender_key()).is_err());
      }
      let message = encrypt_message(&ring, &message_context, PLAINTEXT, &sender(), &mut OsRng).unwrap();
      if let Ok(as_signal) = SealedSignal::from_bytes(&message.to_bytes().unwrap()) {
        assert!(open_signal(&ring, &context(), &as_signal, &sender_key()).is_err());
      }
    }

    #[test]
    fn rejects_unknown_version_and_malformed_bytes() {
      let ring = ring_with_key();
      let mut sealed = seal_signal(&ring, &context(), PLAINTEXT, &sender(), &mut OsRng).unwrap();
      sealed.version = 9;
      assert!(matches!(
        open_signal(&ring, &context(), &sealed, &sender_key()),
        Err(SignalError::UnsupportedVersion(9))
      ));
      assert!(SealedSignal::from_bytes(&[]).is_err());
      assert!(SealedSignal::from_bytes(&[1; 10]).is_err());
    }
  }

  fn sender() -> DeviceKeyPair {
    DeviceKeyPair::from_secrets(
      UserId::new(1),
      DeviceId::new(10),
      &crate::secret::SymmetricKey::from_bytes([3; 32]),
      &crate::secret::SymmetricKey::from_bytes([4; 32]),
    )
  }

  fn sender_key() -> DeviceSigningPublicKey {
    sender().public_keys().signing_key
  }

  fn ring_with_key() -> ContentKeyRing {
    let mut ring = ContentKeyRing::new(CONVERSATION);
    ring.rotate(&mut OsRng);
    ring
  }

  fn context() -> SignalContext {
    SignalContext {
      conversation_id: CONVERSATION,
      sender_user_id: UserId::new(1),
      sender_device_id: DeviceId::new(10),
    }
  }
}
