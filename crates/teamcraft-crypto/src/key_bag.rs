//! The key bag (e2ee.md §2, §6): everything a fresh device needs to read the user's history.
//!
//! Holds the identity key and the content keys of every conversation, serialized and encrypted
//! with the backup key ([`crate::backup`]) and stored on the server. Messages are already on the
//! server as ciphertext, so the key bag is all that's needed to restore a device.
//!
//! Version 2 adds the user's *account data* (saved items, drafts, saved searches; e2ee.md §6.4): opaque
//! bytes to this crate, encrypted together with the keys, so every device of the user sees the same.
//! Version 1 bags still decode (with empty account data).
//!
//! The encrypted form (version 2) carries a *sequence number* chosen by the uploading device and
//! binds it, plus a hash of the user's wrapped backup keys, into the AEAD's associated data. Devices
//! keep the highest sequence they saw and refuse older bags, so the server can't roll a bag back;
//! and it can't drop or swap wraps (a passkey, the org wrap) without the bag failing to open
//! (security review B11).

use std::collections::BTreeMap;
use std::fmt;

use rand_core::CryptoRngCore;
use sha2::{Digest, Sha256};
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
pub const KEY_BAG_VERSION: u8 = 2;
/// The first encoding, without account data.
const KEY_BAG_VERSION_1: u8 = 1;
/// Encrypted form with a sequence number and the wrapped-keys hash in the associated data.
const ENCRYPTED_KEY_BAG_VERSION: u8 = 2;
/// The first encrypted form, bound only to the user.
const ENCRYPTED_KEY_BAG_VERSION_1: u8 = 1;

/// Decrypted key bag of one user.
#[derive(Debug)]
pub struct KeyBag {
  user_id: UserId,
  identity: IdentityKeyPair,
  conversations: BTreeMap<ConversationId, ContentKeyRing>,
  account_data: Zeroizing<Vec<u8>>,
}

/// A key bag encrypted with the backup key, as stored on the server.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct EncryptedKeyBag {
  version: u8,
  /// Chosen by the uploading device, higher than any it saw (0 in version 1).
  sequence: u64,
  nonce: [u8; NONCE_LEN],
  ciphertext: Vec<u8>,
}

/// What an encrypted key bag is bound to besides its owner.
#[derive(Clone, Copy, Debug)]
pub struct KeyBagBinding<'a> {
  pub sequence: u64,
  /// The user's wrapped backup keys exactly as uploaded next to the bag (empty for local copies).
  pub wrapped_keys: &'a [u8],
}

/// Version 1 of the encrypted form.
#[derive(TlsDeserializeBytes, TlsSize)]
struct EncryptedKeyBagV1 {
  version: u8,
  nonce: [u8; NONCE_LEN],
  ciphertext: Vec<u8>,
}

/// Plaintext encoding of the key bag (version 2). Zeroized on drop.
#[derive(Zeroize, ZeroizeOnDrop, TlsSerialize, TlsDeserializeBytes, TlsSize)]
struct KeyBagWire {
  #[zeroize(skip)]
  version: u8,
  #[zeroize(skip)]
  user_id: UserId,
  identity_secret: [u8; SYMMETRIC_KEY_LEN],
  conversations: Vec<ConversationKeysWire>,
  account_data: Vec<u8>,
}

/// Version 1 of the plaintext encoding, read for bags uploaded before account data existed.
#[derive(Zeroize, ZeroizeOnDrop, TlsDeserializeBytes, TlsSize)]
struct KeyBagWireV1 {
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

/// Associated data of the encrypted key bag (version 2).
#[derive(TlsSerialize, TlsSize)]
struct KeyBagAad {
  version: u8,
  user_id: UserId,
  sequence: u64,
  wrapped_keys_hash: [u8; 32],
}

/// Associated data of version 1: binds the bag to its owner only.
#[derive(TlsSerialize, TlsSize)]
struct KeyBagAadV1 {
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
      account_data: Zeroizing::new(Vec::new()),
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

  /// The user's account data (opaque here; teamcraft-core defines and merges it).
  pub fn account_data(&self) -> &[u8] {
    &self.account_data
  }

  pub fn set_account_data(&mut self, account_data: Vec<u8>) {
    self.account_data = Zeroizing::new(account_data);
  }

  /// Serializes and encrypts the key bag with the backup key.
  pub fn encrypt(
    &self,
    backup_key: &BackupKey,
    binding: KeyBagBinding<'_>,
    rng: &mut impl CryptoRngCore,
  ) -> Result<EncryptedKeyBag, KeyBagError> {
    let plaintext = Zeroizing::new(self.to_wire().tls_serialize_detached()?);
    let aad = key_bag_aad(self.user_id, binding)?;
    let (nonce, ciphertext) =
      aead::seal(backup_key.key(), &aad, &plaintext, rng).map_err(|_| KeyBagError::EncryptionFailed)?;
    Ok(EncryptedKeyBag {
      version: ENCRYPTED_KEY_BAG_VERSION,
      sequence: binding.sequence,
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
      account_data: self.account_data.to_vec(),
    }
  }

  /// Decodes either version of the plaintext.
  fn decode(plaintext: &[u8], user_id: UserId) -> Result<Self, KeyBagError> {
    match plaintext.first() {
      Some(&KEY_BAG_VERSION) => {
        let wire = KeyBagWire::tls_deserialize_exact_bytes(plaintext)?;
        check_owner(wire.user_id, user_id)?;
        let mut key_bag = Self::from_parts(wire.user_id, &wire.identity_secret, &wire.conversations)?;
        key_bag.account_data = Zeroizing::new(wire.account_data.clone());
        Ok(key_bag)
      },
      Some(&KEY_BAG_VERSION_1) => {
        let wire = KeyBagWireV1::tls_deserialize_exact_bytes(plaintext)?;
        check_owner(wire.user_id, user_id)?;
        Self::from_parts(wire.user_id, &wire.identity_secret, &wire.conversations)
      },
      Some(&other) => Err(KeyBagError::UnsupportedVersion(other)),
      None => Err(KeyBagError::DecryptionFailed),
    }
  }

  fn from_parts(
    user_id: UserId,
    identity_secret: &[u8; SYMMETRIC_KEY_LEN],
    conversations: &[ConversationKeysWire],
  ) -> Result<Self, KeyBagError> {
    let identity = IdentityKeyPair::from_secret(&SymmetricKey::from_bytes(*identity_secret));
    let mut key_bag = Self::new(user_id, identity);
    for conversation in conversations {
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
  /// Decrypts the key bag of `user_id`. Fails if the bag belongs to someone else, or if
  /// `wrapped_keys` (the wraps the server stores next to it) aren't the ones it was uploaded with.
  pub fn decrypt(
    &self,
    user_id: UserId,
    backup_key: &BackupKey,
    wrapped_keys: &[u8],
  ) -> Result<KeyBag, KeyBagError> {
    let aad = match self.version {
      ENCRYPTED_KEY_BAG_VERSION => key_bag_aad(
        user_id,
        KeyBagBinding {
          sequence: self.sequence,
          wrapped_keys,
        },
      )?,
      ENCRYPTED_KEY_BAG_VERSION_1 => key_bag_aad_v1(user_id)?,
      other => return Err(KeyBagError::UnsupportedVersion(other)),
    };
    let plaintext = aead::open(backup_key.key(), &self.nonce, &aad, &self.ciphertext)
      .map_err(|_| KeyBagError::DecryptionFailed)?;
    KeyBag::decode(plaintext.expose_secret(), user_id)
  }

  /// The uploader's sequence number; 0 for version 1 bags, which have none.
  pub fn sequence(&self) -> u64 {
    self.sequence
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, KeyBagError> {
    Ok(self.tls_serialize_detached()?)
  }

  pub fn from_bytes(bytes: &[u8]) -> Result<Self, KeyBagError> {
    if bytes.first() == Some(&ENCRYPTED_KEY_BAG_VERSION_1) {
      let v1 = EncryptedKeyBagV1::tls_deserialize_exact_bytes(bytes)?;
      return Ok(Self {
        version: v1.version,
        sequence: 0,
        nonce: v1.nonce,
        ciphertext: v1.ciphertext,
      });
    }
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

impl fmt::Debug for KeyBagWireV1 {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("KeyBagWireV1")
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

fn check_owner(actual: UserId, expected: UserId) -> Result<(), KeyBagError> {
  if actual == expected {
    Ok(())
  } else {
    Err(KeyBagError::WrongUser { expected, actual })
  }
}

fn key_bag_aad(user_id: UserId, binding: KeyBagBinding<'_>) -> Result<Vec<u8>, KeyBagError> {
  let aad = KeyBagAad {
    version: ENCRYPTED_KEY_BAG_VERSION,
    user_id,
    sequence: binding.sequence,
    wrapped_keys_hash: Sha256::digest(binding.wrapped_keys).into(),
  };
  Ok(domain::labeled(
    domain::KEY_BAG_AAD,
    &aad.tls_serialize_detached()?,
  ))
}

fn key_bag_aad_v1(user_id: UserId) -> Result<Vec<u8>, KeyBagError> {
  let aad = KeyBagAadV1 {
    version: ENCRYPTED_KEY_BAG_VERSION_1,
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
  const WRAPS: &[u8] = b"passphrase wrap and passkey wrap";
  const BINDING: KeyBagBinding<'static> = KeyBagBinding {
    sequence: 7,
    wrapped_keys: WRAPS,
  };

  #[test]
  fn round_trip_restores_identity_and_all_keys() {
    let key_bag = sample_key_bag();
    let backup_key = BackupKey::generate(&mut OsRng);
    let encrypted = key_bag.encrypt(&backup_key, BINDING, &mut OsRng).unwrap();
    let decoded = EncryptedKeyBag::from_bytes(&encrypted.to_bytes().unwrap()).unwrap();
    let restored = decoded.decrypt(ALICE, &backup_key, WRAPS).unwrap();

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
  fn account_data_round_trips() {
    let mut key_bag = sample_key_bag();
    key_bag.set_account_data(b"{\"saved\":[]}".to_vec());
    let backup_key = BackupKey::generate(&mut OsRng);
    let restored = key_bag
      .encrypt(&backup_key, BINDING, &mut OsRng)
      .unwrap()
      .decrypt(ALICE, &backup_key, WRAPS)
      .unwrap();
    assert_eq!(restored.account_data(), b"{\"saved\":[]}");
  }

  #[test]
  fn version_1_bags_still_open_without_account_data() {
    let key_bag = sample_key_bag();
    // Version 1 is the version 2 layout without the trailing account data (empty: one length byte).
    let mut plaintext = key_bag.to_wire().tls_serialize_detached().unwrap();
    plaintext.truncate(plaintext.len() - 1);
    plaintext[0] = KEY_BAG_VERSION_1;
    let restored = KeyBag::decode(&plaintext, ALICE).unwrap();
    assert!(restored.account_data().is_empty());
    assert_eq!(restored.conversations().count(), key_bag.conversations().count());
    assert_eq!(restored.identity().public_key(), key_bag.identity().public_key());
    assert!(matches!(
      KeyBag::decode(&plaintext, UserId::new(2)),
      Err(KeyBagError::WrongUser { .. })
    ));
  }

  #[test]
  fn version_1_encrypted_bags_still_open() {
    let key_bag = sample_key_bag();
    let backup_key = BackupKey::generate(&mut OsRng);
    let plaintext = key_bag.to_wire().tls_serialize_detached().unwrap();
    let (nonce, ciphertext) = aead::seal(
      backup_key.key(),
      &key_bag_aad_v1(ALICE).unwrap(),
      &plaintext,
      &mut OsRng,
    )
    .unwrap();
    let v1 = EncryptedKeyBagV1Writer {
      version: ENCRYPTED_KEY_BAG_VERSION_1,
      nonce,
      ciphertext,
    }
    .tls_serialize_detached()
    .unwrap();
    let decoded = EncryptedKeyBag::from_bytes(&v1).unwrap();
    assert_eq!(decoded.sequence(), 0);
    let restored = decoded.decrypt(ALICE, &backup_key, b"ignored").unwrap();
    assert_eq!(restored.conversations().count(), 2);
  }

  #[derive(TlsSerialize, TlsSize)]
  struct EncryptedKeyBagV1Writer {
    version: u8,
    nonce: [u8; NONCE_LEN],
    ciphertext: Vec<u8>,
  }

  #[test]
  fn empty_key_bag_round_trips() {
    let key_bag = KeyBag::new(ALICE, IdentityKeyPair::generate(&mut OsRng));
    let backup_key = BackupKey::generate(&mut OsRng);
    let restored = key_bag
      .encrypt(&backup_key, BINDING, &mut OsRng)
      .unwrap()
      .decrypt(ALICE, &backup_key, WRAPS)
      .unwrap();
    assert_eq!(restored.conversations().count(), 0);
  }

  mod rejection {
    use super::*;

    #[test]
    fn rejects_wrong_backup_key() {
      let encrypted = sample_key_bag()
        .encrypt(&BackupKey::generate(&mut OsRng), BINDING, &mut OsRng)
        .unwrap();
      assert!(matches!(
        encrypted.decrypt(ALICE, &BackupKey::generate(&mut OsRng), WRAPS),
        Err(KeyBagError::DecryptionFailed)
      ));
    }

    #[test]
    fn rejects_other_users_key_bag() {
      // The server hands Bob's (or an old) bag to Alice's device: the AAD doesn't match.
      let backup_key = BackupKey::generate(&mut OsRng);
      let encrypted = sample_key_bag()
        .encrypt(&backup_key, BINDING, &mut OsRng)
        .unwrap();
      assert!(matches!(
        encrypted.decrypt(UserId::new(2), &backup_key, WRAPS),
        Err(KeyBagError::DecryptionFailed)
      ));
    }

    #[test]
    fn rejects_tampering() {
      let backup_key = BackupKey::generate(&mut OsRng);
      let encrypted = sample_key_bag()
        .encrypt(&backup_key, BINDING, &mut OsRng)
        .unwrap();
      let mut body = encrypted.clone();
      body.ciphertext[10] ^= 1;
      let mut nonce = encrypted.clone();
      nonce.nonce[0] ^= 1;
      let mut version = encrypted.clone();
      version.version = 3;
      let mut sequence = encrypted;
      sequence.sequence -= 1;

      assert!(matches!(
        body.decrypt(ALICE, &backup_key, WRAPS),
        Err(KeyBagError::DecryptionFailed)
      ));
      assert!(matches!(
        nonce.decrypt(ALICE, &backup_key, WRAPS),
        Err(KeyBagError::DecryptionFailed)
      ));
      assert!(matches!(
        version.decrypt(ALICE, &backup_key, WRAPS),
        Err(KeyBagError::UnsupportedVersion(3))
      ));
      // The server rolls the sequence back or drops a wrap: the bag no longer opens.
      assert!(matches!(
        sequence.decrypt(ALICE, &backup_key, WRAPS),
        Err(KeyBagError::DecryptionFailed)
      ));
      assert!(matches!(
        body_for_wraps(&backup_key).decrypt(ALICE, &backup_key, b"only the passphrase wrap"),
        Err(KeyBagError::DecryptionFailed)
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
        encrypt_wire(&wire, &backup_key).decrypt(ALICE, &backup_key, WRAPS),
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
        encrypt_wire(&duplicate, &backup_key).decrypt(ALICE, &backup_key, WRAPS),
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
        &key_bag_aad(ALICE, BINDING).unwrap(),
        &plaintext,
        &mut OsRng,
      )
      .unwrap();
      EncryptedKeyBag {
        version: ENCRYPTED_KEY_BAG_VERSION,
        sequence: BINDING.sequence,
        nonce,
        ciphertext,
      }
    }

    fn body_for_wraps(backup_key: &BackupKey) -> EncryptedKeyBag {
      sample_key_bag().encrypt(backup_key, BINDING, &mut OsRng).unwrap()
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
