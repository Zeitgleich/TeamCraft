//! Conversation content keys (e2ee.md §4.1–§4.3).
//!
//! Each conversation has a chain of symmetric content keys. The current key encrypts new messages;
//! older keys stay in the ring so history remains readable. A new key is generated on rotation and
//! distributed through MLS ([`crate::mls`]); new members receive past keys per history policy.

use std::fmt;
use std::time::Duration;

use rand_core::CryptoRngCore;
use tls_codec::{TlsDeserializeBytes, TlsSerialize, TlsSize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::ids::ConversationId;
use crate::secret::{SYMMETRIC_KEY_LEN, SymmetricKey};

/// Default for [`RotationPolicy::max_messages`].
pub const DEFAULT_ROTATE_AFTER_MESSAGES: u32 = 10_000;
/// Default for [`RotationPolicy::max_age`]: 7 days (e2ee.md §4.2).
pub const DEFAULT_ROTATE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// All content keys of one conversation that this device holds, oldest first.
#[derive(Debug)]
pub struct ContentKeyRing {
  conversation_id: ConversationId,
  keys: Vec<ContentKey>,
  maybe_current: Option<ContentKeyId>,
}

/// Public identifier of a content key, sent in plaintext with every message (e2ee.md §4.2).
///
/// Random rather than sequential: two members may rotate concurrently (e.g. both reach the message
/// limit), and random IDs can't collide in practice, so both keys stay usable.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct ContentKeyId([u8; 16]);

/// One content key with its ID.
#[derive(Debug)]
pub struct ContentKey {
  id: ContentKeyId,
  key: SymmetricKey,
}

/// When the sending client rotates the current key, besides member and device removal, which
/// always rotate (e2ee.md §4.2). Configurable; defaults per the design doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RotationPolicy {
  pub max_messages: u32,
  pub max_age: Duration,
}

/// Why a rotation is due.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotationReason {
  MemberRemoved,
  DeviceRemoved,
  MessageLimit,
  KeyAge,
}

/// Wire form of a content key inside MLS application messages and the key bag. Zeroized on drop.
#[derive(Zeroize, ZeroizeOnDrop, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub(crate) struct ContentKeyWire {
  #[zeroize(skip)]
  pub(crate) id: ContentKeyId,
  pub(crate) key: [u8; SYMMETRIC_KEY_LEN],
}

#[derive(Debug, thiserror::Error)]
pub enum ContentKeyError {
  #[error("content key {0:?} is already in the ring with different key material")]
  ConflictingKey(ContentKeyId),
  #[error("content key {0:?} is not in the ring")]
  UnknownKey(ContentKeyId),
}

impl ContentKeyRing {
  pub fn new(conversation_id: ConversationId) -> Self {
    Self {
      conversation_id,
      keys: Vec::new(),
      maybe_current: None,
    }
  }

  pub fn conversation_id(&self) -> ConversationId {
    self.conversation_id
  }

  /// Generates a new key and makes it current. The caller distributes it via MLS.
  pub fn rotate(&mut self, rng: &mut impl CryptoRngCore) -> &ContentKey {
    let key = ContentKey::generate(rng);
    self.maybe_current = Some(key.id);
    let index = self.keys.len();
    self.keys.push(key);
    &self.keys[index]
  }

  /// Adds a key received from another member. Re-adding the same key is a no-op; a different key
  /// under a known ID is rejected, so a malicious member can't replace history keys.
  pub fn insert(&mut self, key: ContentKey) -> Result<(), ContentKeyError> {
    match self.get(key.id) {
      Some(existing) if existing.has_same_material(&key) => Ok(()),
      Some(_) => Err(ContentKeyError::ConflictingKey(key.id)),
      None => {
        self.keys.push(key);
        Ok(())
      },
    }
  }

  /// Marks a key already in the ring as the one to encrypt new messages with.
  pub fn set_current(&mut self, id: ContentKeyId) -> Result<(), ContentKeyError> {
    if self.get(id).is_none() {
      return Err(ContentKeyError::UnknownKey(id));
    }
    self.maybe_current = Some(id);
    Ok(())
  }

  /// Stops encrypting with `id` if it is the current key; the key stays in the ring, so what was
  /// encrypted with it can still be read. The next key that arrives becomes current.
  pub fn unset_current(&mut self, id: ContentKeyId) {
    if self.maybe_current == Some(id) {
      self.maybe_current = None;
    }
  }

  pub fn current(&self) -> Option<&ContentKey> {
    self.maybe_current.and_then(|id| self.get(id))
  }

  pub fn get(&self, id: ContentKeyId) -> Option<&ContentKey> {
    self.keys.iter().find(|key| key.id == id)
  }

  /// All keys, oldest first; for sharing history with a new member (e2ee.md §4.3).
  pub fn keys(&self) -> &[ContentKey] {
    &self.keys
  }

  pub fn len(&self) -> usize {
    self.keys.len()
  }

  pub fn is_empty(&self) -> bool {
    self.keys.is_empty()
  }
}

impl ContentKeyId {
  pub fn from_bytes(bytes: [u8; 16]) -> Self {
    Self(bytes)
  }

  pub fn as_bytes(&self) -> &[u8; 16] {
    &self.0
  }

  fn generate(rng: &mut impl CryptoRngCore) -> Self {
    let mut bytes = [0u8; 16];
    rng.fill_bytes(&mut bytes);
    Self(bytes)
  }
}

impl fmt::Debug for ContentKeyId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("ContentKeyId(")?;
    for byte in self.0 {
      write!(f, "{byte:02x}")?;
    }
    f.write_str(")")
  }
}

impl ContentKey {
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    Self {
      id: ContentKeyId::generate(rng),
      key: SymmetricKey::generate(rng),
    }
  }

  pub fn from_parts(id: ContentKeyId, key: SymmetricKey) -> Self {
    Self { id, key }
  }

  pub fn id(&self) -> ContentKeyId {
    self.id
  }

  pub(crate) fn key(&self) -> &SymmetricKey {
    &self.key
  }

  pub fn duplicate(&self) -> Self {
    Self {
      id: self.id,
      key: self.key.duplicate(),
    }
  }

  pub(crate) fn to_wire(&self) -> ContentKeyWire {
    ContentKeyWire {
      id: self.id,
      key: *self.key.expose_secret(),
    }
  }

  pub(crate) fn from_wire(wire: &ContentKeyWire) -> Self {
    Self {
      id: wire.id,
      key: SymmetricKey::from_bytes(wire.key),
    }
  }

  fn has_same_material(&self, other: &Self) -> bool {
    use subtle::ConstantTimeEq;
    bool::from(self.key.ct_eq(&other.key))
  }
}

impl RotationPolicy {
  /// Whether the current key should be replaced before sending the next message.
  pub fn due(&self, messages_with_current_key: u32, current_key_age: Duration) -> Option<RotationReason> {
    if messages_with_current_key >= self.max_messages {
      Some(RotationReason::MessageLimit)
    } else if current_key_age >= self.max_age {
      Some(RotationReason::KeyAge)
    } else {
      None
    }
  }
}

impl Default for RotationPolicy {
  fn default() -> Self {
    Self {
      max_messages: DEFAULT_ROTATE_AFTER_MESSAGES,
      max_age: DEFAULT_ROTATE_AFTER,
    }
  }
}

impl fmt::Debug for ContentKeyWire {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ContentKeyWire")
      .field("id", &self.id)
      .finish_non_exhaustive()
  }
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  const CONVERSATION: ConversationId = ConversationId::new(7);

  #[test]
  fn rotate_makes_new_key_current_and_keeps_old_ones() {
    let mut ring = ContentKeyRing::new(CONVERSATION);
    assert!(ring.current().is_none());
    let first = ring.rotate(&mut OsRng).id();
    let second = ring.rotate(&mut OsRng).id();

    assert_ne!(first, second);
    assert_eq!(ring.current().map(ContentKey::id), Some(second));
    assert!(ring.get(first).is_some());
    assert_eq!(
      ring.keys().iter().map(ContentKey::id).collect::<Vec<_>>(),
      vec![first, second]
    );
  }

  #[test]
  fn insert_is_idempotent_but_rejects_conflicting_material() {
    let mut ring = ContentKeyRing::new(CONVERSATION);
    let key = ContentKey::generate(&mut OsRng);
    ring.insert(key.duplicate()).unwrap();
    ring.insert(key.duplicate()).unwrap();
    assert_eq!(ring.len(), 1);

    let conflicting = ContentKey::from_parts(key.id(), SymmetricKey::generate(&mut OsRng));
    assert!(matches!(
      ring.insert(conflicting),
      Err(ContentKeyError::ConflictingKey(_))
    ));
  }

  #[test]
  fn unset_current_only_clears_that_key_and_keeps_it() {
    let mut ring = ContentKeyRing::new(CONVERSATION);
    let first = ring.rotate(&mut OsRng).id();
    let second = ring.rotate(&mut OsRng).id();

    ring.unset_current(first);
    assert_eq!(
      ring.current().map(ContentKey::id),
      Some(second),
      "not current: no-op"
    );
    ring.unset_current(second);
    assert!(ring.current().is_none());
    assert!(ring.get(second).is_some(), "still there for decryption");
  }

  #[test]
  fn set_current_requires_known_key() {
    let mut ring = ContentKeyRing::new(CONVERSATION);
    let unknown = ContentKey::generate(&mut OsRng).id();
    assert!(matches!(
      ring.set_current(unknown),
      Err(ContentKeyError::UnknownKey(_))
    ));
  }

  #[test]
  fn rotation_policy_triggers_on_count_and_age() {
    let policy = RotationPolicy {
      max_messages: 3,
      max_age: Duration::from_secs(60),
    };
    assert_eq!(policy.due(2, Duration::from_secs(59)), None);
    assert_eq!(policy.due(3, Duration::ZERO), Some(RotationReason::MessageLimit));
    assert_eq!(
      policy.due(0, Duration::from_secs(60)),
      Some(RotationReason::KeyAge)
    );
    assert_eq!(RotationPolicy::default().max_age, Duration::from_secs(604_800));
  }

  #[test]
  fn debug_hides_key_material() {
    let key = ContentKey::generate(&mut OsRng);
    let output = format!("{:?} {:?}", key, key.to_wire());
    assert!(output.contains("[redacted]"));
    assert!(!output.contains("key: ["));
  }
}
