//! Content key bundles: the payload of TeamCraft's MLS application messages (e2ee.md §4.2, §4.3).

use rand_core::CryptoRngCore;
use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::MlsError;
use crate::content_key::{ContentKey, ContentKeyError, ContentKeyId, ContentKeyRing, ContentKeyWire};
use crate::ids::ConversationId;
use crate::secret::SecretBytes;

const BUNDLE_VERSION: u8 = 1;

/// Content keys sent to all group members, optionally marking one as the new current key.
#[derive(Debug)]
pub struct ContentKeyBundle {
  conversation_id: ConversationId,
  maybe_current: Option<ContentKeyId>,
  keys: Vec<ContentKey>,
}

#[derive(Zeroize, ZeroizeOnDrop, TlsSerialize, TlsDeserializeBytes, TlsSize)]
struct BundleWire {
  #[zeroize(skip)]
  version: u8,
  #[zeroize(skip)]
  conversation_id: ConversationId,
  #[zeroize(skip)]
  maybe_current: Option<ContentKeyId>,
  keys: Vec<ContentKeyWire>,
}

impl ContentKeyBundle {
  /// Every key of the ring for a member added with full history (e2ee.md §4.3). With history
  /// from the join on, the new member gets only the epoch key ([`Self::epoch_key`]).
  pub fn full_history(ring: &mut ContentKeyRing, rng: &mut impl CryptoRngCore) -> Self {
    if ring.current().is_none() {
      ring.rotate(rng);
    }
    Self {
      conversation_id: ring.conversation_id(),
      maybe_current: ring.current().map(ContentKey::id),
      keys: ring.keys().iter().map(ContentKey::duplicate).collect(),
    }
  }

  pub fn conversation_id(&self) -> ConversationId {
    self.conversation_id
  }

  pub fn keys(&self) -> &[ContentKey] {
    &self.keys
  }

  pub fn current(&self) -> Option<ContentKeyId> {
    self.maybe_current
  }

  /// Adds the keys to the ring. The bundle's current key is taken only by a ring that has none
  /// yet (a new member): a member's current key changes only with the epoch (see
  /// [`super::ConversationGroup::epoch_content_key`]), so an old or delayed bundle can't move it
  /// back to a key a removed member knows (security review A5).
  pub fn apply_to(self, ring: &mut ContentKeyRing) -> Result<(), ContentKeyError> {
    for key in self.keys {
      ring.insert(key)?;
    }
    if let Some(current) = self.maybe_current
      && ring.current().is_none()
    {
      ring.set_current(current)?;
    }
    Ok(())
  }

  /// Hands a new member the epoch's key as current (history from its join on, e2ee.md §4.3); the
  /// existing members derived the same key themselves.
  pub fn epoch_key(conversation_id: ConversationId, key: ContentKey) -> Self {
    Self {
      conversation_id,
      maybe_current: Some(key.id()),
      keys: vec![key],
    }
  }

  pub(super) fn to_bytes(&self) -> Result<SecretBytes, MlsError> {
    let wire = BundleWire {
      version: BUNDLE_VERSION,
      conversation_id: self.conversation_id,
      maybe_current: self.maybe_current,
      keys: self.keys.iter().map(ContentKey::to_wire).collect(),
    };
    Ok(SecretBytes::from(Zeroizing::new(wire.tls_serialize_detached()?)))
  }

  pub(super) fn from_bytes(bytes: &[u8]) -> Result<Self, MlsError> {
    let wire = BundleWire::tls_deserialize_exact_bytes(bytes)?;
    if wire.version != BUNDLE_VERSION {
      return Err(MlsError::UnexpectedMessage(
        "content key bundle with unknown version",
      ));
    }
    let maybe_current = wire.maybe_current;
    if maybe_current.is_some_and(|current| !wire.keys.iter().any(|key| key.id == current)) {
      return Err(MlsError::UnexpectedMessage(
        "content key bundle marks a key it doesn't contain",
      ));
    }
    let keys = wire.keys.iter().map(ContentKey::from_wire).collect();
    Ok(Self {
      conversation_id: wire.conversation_id,
      maybe_current,
      keys,
    })
  }
}

impl std::fmt::Debug for BundleWire {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("BundleWire")
      .field("conversation_id", &self.conversation_id)
      .finish_non_exhaustive()
  }
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  const CONVERSATION: ConversationId = ConversationId::new(9);

  #[test]
  fn a_bundle_cannot_move_the_current_key_back() {
    // Security review A5: a delayed full-history bundle names an older key as current.
    let mut sender = ContentKeyRing::new(CONVERSATION);
    let old = sender.rotate(&mut OsRng).id();
    let stale = ContentKeyBundle::full_history(&mut sender, &mut OsRng);
    let mut member = ContentKeyRing::new(CONVERSATION);
    ContentKeyBundle::full_history(&mut sender, &mut OsRng)
      .apply_to(&mut member)
      .unwrap();
    let newer = ContentKey::generate(&mut OsRng);
    let newer_id = newer.id();
    member.insert(newer).unwrap();
    member.set_current(newer_id).unwrap();

    stale.apply_to(&mut member).unwrap();
    assert_eq!(member.current().map(ContentKey::id), Some(newer_id));
    assert!(member.get(old).is_some(), "the keys themselves are still added");
  }

  #[test]
  fn full_history_shares_all_keys() {
    let mut ring = ContentKeyRing::new(CONVERSATION);
    ring.rotate(&mut OsRng);
    ring.rotate(&mut OsRng);
    let bundle = ContentKeyBundle::full_history(&mut ring, &mut OsRng);
    assert_eq!(bundle.keys().len(), 2);

    let decoded = ContentKeyBundle::from_bytes(bundle.to_bytes().unwrap().expose_secret()).unwrap();
    let mut new_members_ring = ContentKeyRing::new(CONVERSATION);
    decoded.apply_to(&mut new_members_ring).unwrap();
    assert_eq!(new_members_ring.len(), 2);
    assert_eq!(
      new_members_ring.current().map(ContentKey::id),
      ring.current().map(ContentKey::id)
    );
  }

  #[test]
  fn rejects_bundle_with_dangling_current_key() {
    let wire = BundleWire {
      version: BUNDLE_VERSION,
      conversation_id: CONVERSATION,
      maybe_current: Some(ContentKey::generate(&mut OsRng).id()),
      keys: vec![ContentKey::generate(&mut OsRng).to_wire()],
    };
    let bytes = wire.tls_serialize_detached().unwrap();
    assert!(matches!(
      ContentKeyBundle::from_bytes(&bytes),
      Err(MlsError::UnexpectedMessage(_))
    ));
    assert!(ContentKeyBundle::from_bytes(&[]).is_err());
  }
}
