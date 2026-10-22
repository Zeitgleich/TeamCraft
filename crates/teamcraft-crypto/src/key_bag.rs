//! The key bag (e2ee.md §2, §6): everything a fresh device needs to read the user's history.
//!
//! Holds the identity key and the content keys of every conversation, serialized and encrypted
//! with the backup key ([`crate::backup`]) and stored on the server. Messages are already on the
//! server as ciphertext, so the key bag is all that's needed to restore a device.
//!
//! Not yet included: index keys (§10.1) and settings; they get their own fields when those
//! features land (bump [`KEY_BAG_VERSION`]).

use std::collections::BTreeMap;
use std::fmt;

use rand_core::CryptoRngCore;
use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::aead::{self, NONCE_LEN};
use crate::backup::BackupKey;
use crate::content_key::{ContentKey, ContentKeyError, ContentKeyId, ContentKeyRing, ContentKeyWire};
use crate::domain;
use crate::identity::IdentityKeyPair;
use crate::ids::{ConversationId, UserId};
use crate::secret::{SYMMETRIC_KEY_LEN, SymmetricKey};

/// Version of the plaintext key bag encoding.
pub const KEY_BAG_VERSION: u8 = 1;
const ENCRYPTED_KEY_BAG_VERSION: u8 = 1;

/// Decrypted key bag of one user.
#[derive(Debug)]
pub struct KeyBag {
  user_id: UserId,
  identity: IdentityKeyPair,
  conversations: BTreeMap<ConversationId, ContentKeyRing>,
}

/// A key bag encrypted with the backup key, as stored on the server.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct EncryptedKeyBag {
  version: u8,
  nonce: [u8; NONCE_LEN],
  ciphertext: Vec<u8>,
}

/// Plaintext encoding of the key bag. Zeroized on drop.
#[derive(Zeroize, ZeroizeOnDrop, TlsSerialize, TlsDeserializeBytes, TlsSize)]
struct KeyBagWire {
  #[zeroize(skip)]
  version: u8,
  #[zeroize(skip)]
  user_id: UserId,
  identity_secret: [u8; SYMMETRIC_KEY_LEN],
  conversations: Vec<ConversationKeysWire>,
}

#[derive(Zeroize, ZeroizeOnDrop, TlsSerialize, TlsDeserializeBytes, TlsSize)]
struct ConversationKeysWire {
  #[zeroize(skip)]
  conversation_id: ConversationId,
  #[zeroize(skip)]
  maybe_current: Option<ContentKeyId>,
  keys: Vec<ContentKeyWire>,
}

/// Associated data of the encrypted key bag: binds it to its owner.
#[derive(TlsSerialize, TlsSize)]
struct KeyBagAad {
  version: u8,
  user_id: UserId,
}

#[derive(Debug, thiserror::Error)]
pub enum KeyBagError {
  #[error("unsupported key bag version {0}")]
  UnsupportedVersion(u8),
  #[error("key bag belongs to user {actual}, expected {expected}")]
  WrongUser { expected: UserId, actual: UserId },
  #[error("key bag lists conversation {0} twice")]
  DuplicateConversation(ConversationId),
  #[error("key bag contains inconsistent content keys: {0}")]
  InvalidContentKeys(#[from] ContentKeyError),
  #[error("encryption failed")]
  EncryptionFailed,
  #[error("key bag could not be decrypted (wrong backup key, wrong user or tampered)")]
  DecryptionFailed,
  #[error("encoding failed: {0}")]
  Encoding(#[from] tls_codec::Error),
}

impl KeyBag {
  pub fn new(user_id: UserId, identity: IdentityKeyPair) -> Self {
    Self {
      user_id,
      identity,
      conversations: BTreeMap::new(),
    }
  }

  pub fn user_id(&self) -> UserId {
    self.user_id
  }

  pub fn identity(&self) -> &IdentityKeyPair {
    &self.identity
  }

  pub fn ring(&self, conversation_id: ConversationId) -> Option<&ContentKeyRing> {
    self.conversations.get(&conversation_id)
  }

  /// The ring of a conversation, created empty on first use.
  pub fn ring_mut(&mut self, conversation_id: ConversationId) -> &mut ContentKeyRing {
    self
      .conversations
      .entry(conversation_id)
      .or_insert_with(|| ContentKeyRing::new(conversation_id))
  }

  pub fn conversations(&self) -> impl Iterator<Item = &ContentKeyRing> {
    self.conversations.values()
  }

  /// Serializes and encrypts the key bag with the backup key.
  pub fn encrypt(
    &self,
    backup_key: &BackupKey,
    rng: &mut impl CryptoRngCore,
  ) -> Result<EncryptedKeyBag, KeyBagError> {
    let plaintext = Zeroizing::new(self.to_wire().tls_serialize_detached()?);
    let aad = key_bag_aad(self.user_id)?;
    let (nonce, ciphertext) =
      aead::seal(backup_key.key(), &aad, &plaintext, rng).map_err(|_| KeyBagError::EncryptionFailed)?;
    Ok(EncryptedKeyBag {
      version: ENCRYPTED_KEY_BAG_VERSION,
      nonce,
      ciphertext,
    })
  }

  fn to_wire(&self) -> KeyBagWire {
    let conversations = self
      .conversations
      .values()
      .map(|ring| ConversationKeysWire {
        conversation_id: ring.conversation_id(),
        maybe_current: ring.current().map(ContentKey::id),
        keys: ring.keys().iter().map(ContentKey::to_wire).collect(),
      })
      .collect();
    KeyBagWire {
      version: KEY_BAG_VERSION,
      user_id: self.user_id,
      identity_secret: *self.identity.to_secret().expose_secret(),
      conversations,
    }
  }

  fn from_wire(wire: &KeyBagWire) -> Result<Self, KeyBagError> {
    if wire.version != KEY_BAG_VERSION {
      return Err(KeyBagError::UnsupportedVersion(wire.version));
    }
    let identity = IdentityKeyPair::from_secret(&SymmetricKey::from_bytes(wire.identity_secret));
    let mut key_bag = Self::new(wire.user_id, identity);
    for conversation in &wire.conversations {
      if key_bag.conversations.contains_key(&conversation.conversation_id) {
        return Err(KeyBagError::DuplicateConversation(conversation.conversation_id));
      }
      let ring = key_bag.ring_mut(conversation.conversation_id);
      for key in &conversation.keys {
        ring.insert(ContentKey::from_wire(key))?;
      }
      if let Some(current) = conversation.maybe_current {
        ring.set_current(current)?;
      }
    }
    Ok(key_bag)
  }
}

impl EncryptedKeyBag {
  /// Decrypts the key bag of `user_id`. Fails if the bag belongs to someone else.
  pub fn decrypt(&self, user_id: UserId, backup_key: &BackupKey) -> Result<KeyBag, KeyBagError> {
    if self.version != ENCRYPTED_KEY_BAG_VERSION {
      return Err(KeyBagError::UnsupportedVersion(self.version));
    }
    let aad = key_bag_aad(user_id)?;
    let plaintext = aead::open(backup_key.key(), &self.nonce, &aad, &self.ciphertext)
      .map_err(|_| KeyBagError::DecryptionFailed)?;
    let wire = KeyBagWire::tls_deserialize_exact_bytes(plaintext.expose_secret())?;
    if wire.user_id != user_id {
      return Err(KeyBagError::WrongUser {
        expected: user_id,
        actual: wire.user_id,
      });
    }
    KeyBag::from_wire(&wire)
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, KeyBagError> {
    Ok(self.tls_serialize_detached()?)
  }

  pub fn from_bytes(bytes: &[u8]) -> Result<Self, KeyBagError> {
    Ok(Self::tls_deserialize_exact_bytes(bytes)?)
  }
}

impl fmt::Debug for KeyBagWire {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("KeyBagWire")
      .field("user_id", &self.user_id)
      .finish_non_exhaustive()
  }
}

impl fmt::Debug for ConversationKeysWire {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ConversationKeysWire")
      .field("conversation_id", &self.conversation_id)
      .finish_non_exhaustive()
  }
}

fn key_bag_aad(user_id: UserId) -> Result<Vec<u8>, KeyBagError> {
  let aad = KeyBagAad {
    version: ENCRYPTED_KEY_BAG_VERSION,
    user_id,
  };
  Ok(domain::labeled(
    domain::KEY_BAG_AAD,
    &aad.tls_serialize_detached()?,
  ))
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use subtle::ConstantTimeEq;

  use super::*;

  const ALICE: UserId = UserId::new(1);
  const GENERAL: ConversationId = ConversationId::new(100);
  const RANDOM: ConversationId = ConversationId::new(101);

  #[test]
  fn round_trip_restores_identity_and_all_keys() {
    let key_bag = sample_key_bag();
    let backup_key = BackupKey::generate(&mut OsRng);
    let encrypted = key_bag.encrypt(&backup_key, &mut OsRng).unwrap();
    let decoded = EncryptedKeyBag::from_bytes(&encrypted.to_bytes().unwrap()).unwrap();
    let restored = decoded.decrypt(ALICE, &backup_key).unwrap();

    assert_eq!(restored.identity().public_key(), key_bag.identity().public_key());
    for conversation in [GENERAL, RANDOM] {
      let original = key_bag.ring(conversation).unwrap();
      let copy = restored.ring(conversation).unwrap();
      assert_eq!(copy.len(), original.len());
      assert_eq!(
        copy.current().map(ContentKey::id),
        original.current().map(ContentKey::id)
      );
      for key in original.keys() {
        assert!(bool::from(copy.get(key.id()).unwrap().key().ct_eq(key.key())));
      }
    }
  }

  #[test]
  fn empty_key_bag_round_trips() {
    let key_bag = KeyBag::new(ALICE, IdentityKeyPair::generate(&mut OsRng));
    let backup_key = BackupKey::generate(&mut OsRng);
    let restored = key_bag
      .encrypt(&backup_key, &mut OsRng)
      .unwrap()
      .decrypt(ALICE, &backup_key)
      .unwrap();
    assert_eq!(restored.conversations().count(), 0);
  }

  mod rejection {
    use super::*;

    #[test]
    fn rejects_wrong_backup_key() {
      let encrypted = sample_key_bag()
        .encrypt(&BackupKey::generate(&mut OsRng), &mut OsRng)
        .unwrap();
      assert!(matches!(
        encrypted.decrypt(ALICE, &BackupKey::generate(&mut OsRng)),
        Err(KeyBagError::DecryptionFailed)
      ));
    }

    #[test]
    fn rejects_other_users_key_bag() {
      // The server hands Bob's (or an old) bag to Alice's device: the AAD doesn't match.
      let backup_key = BackupKey::generate(&mut OsRng);
      let encrypted = sample_key_bag().encrypt(&backup_key, &mut OsRng).unwrap();
      assert!(matches!(
        encrypted.decrypt(UserId::new(2), &backup_key),
        Err(KeyBagError::DecryptionFailed)
      ));
    }

    #[test]
    fn rejects_tampering() {
      let backup_key = BackupKey::generate(&mut OsRng);
      let encrypted = sample_key_bag().encrypt(&backup_key, &mut OsRng).unwrap();
      let mut body = encrypted.clone();
      body.ciphertext[10] ^= 1;
      let mut nonce = encrypted.clone();
      nonce.nonce[0] ^= 1;
      let mut version = encrypted;
      version.version = 2;

      assert!(matches!(
        body.decrypt(ALICE, &backup_key),
        Err(KeyBagError::DecryptionFailed)
      ));
      assert!(matches!(
        nonce.decrypt(ALICE, &backup_key),
        Err(KeyBagError::DecryptionFailed)
      ));
      assert!(matches!(
        version.decrypt(ALICE, &backup_key),
        Err(KeyBagError::UnsupportedVersion(2))
      ));
    }

    #[test]
    fn rejects_inconsistent_plaintext() {
      // Validly encrypted but malformed content (e.g. from a buggy client) is still rejected.
      let backup_key = BackupKey::generate(&mut OsRng);
      let key = ContentKey::generate(&mut OsRng);
      let conflicting = ContentKey::from_parts(key.id(), SymmetricKey::generate(&mut OsRng));
      let mut wire = sample_key_bag().to_wire();
      wire.conversations[0].keys.push(conflicting.to_wire());
      wire.conversations[0].keys.push(key.to_wire());
      assert!(matches!(
        encrypt_wire(&wire, &backup_key).decrypt(ALICE, &backup_key),
        Err(KeyBagError::InvalidContentKeys(_))
      ));

      let mut duplicate = sample_key_bag().to_wire();
      let copy = ConversationKeysWire {
        conversation_id: GENERAL,
        maybe_current: None,
        keys: Vec::new(),
      };
      duplicate.conversations.push(copy);
      assert!(matches!(
        encrypt_wire(&duplicate, &backup_key).decrypt(ALICE, &backup_key),
        Err(KeyBagError::DuplicateConversation(GENERAL))
      ));
    }

    #[test]
    fn rejects_malformed_bytes() {
      assert!(EncryptedKeyBag::from_bytes(&[]).is_err());
      assert!(EncryptedKeyBag::from_bytes(&[1; 20]).is_err());
    }

    fn encrypt_wire(wire: &KeyBagWire, backup_key: &BackupKey) -> EncryptedKeyBag {
      let plaintext = wire.tls_serialize_detached().unwrap();
      let (nonce, ciphertext) = aead::seal(
        backup_key.key(),
        &key_bag_aad(ALICE).unwrap(),
        &plaintext,
        &mut OsRng,
      )
      .unwrap();
      EncryptedKeyBag {
        version: ENCRYPTED_KEY_BAG_VERSION,
        nonce,
        ciphertext,
      }
    }
  }

  fn sample_key_bag() -> KeyBag {
    let mut key_bag = KeyBag::new(ALICE, IdentityKeyPair::generate(&mut OsRng));
    key_bag.ring_mut(GENERAL).rotate(&mut OsRng);
    key_bag.ring_mut(GENERAL).rotate(&mut OsRng);
    key_bag
      .ring_mut(RANDOM)
      .insert(ContentKey::generate(&mut OsRng))
      .unwrap();
    key_bag
  }
}
