//! Content key bundles: the payload of TeamCraft's MLS application messages (e2ee.md §4.2, §4.3).

use rand_core::CryptoRngCore;
use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::MlsError;
use crate::content_key::{ContentKey, ContentKeyError, ContentKeyId, ContentKeyRing, ContentKeyWire};
use crate::ids::ConversationId;
use crate::membership::HistoryShare;
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
  /// Rotates the ring and bundles only the new key: after a removal (e2ee.md §4.2) or on schedule.
  pub fn rotate(ring: &mut ContentKeyRing, rng: &mut impl CryptoRngCore) -> Self {
    let key = ring.rotate(rng).duplicate();
    Self {
      conversation_id: ring.conversation_id(),
      maybe_current: Some(key.id()),
      keys: vec![key],
    }
  }

  /// Keys for a newly added member according to the "Share history" choice (e2ee.md §4.3).
  /// `FromJoin` rotates first, so the new member can't read anything sent before it joined.
  pub fn for_new_member(
    ring: &mut ContentKeyRing,
    history: HistoryShare,
    rng: &mut impl CryptoRngCore,
  ) -> Self {
    match history {
      HistoryShare::FromJoin => Self::rotate(ring, rng),
      HistoryShare::Full => {
        if ring.current().is_none() {
          ring.rotate(rng);
        }
        Self {
          conversation_id: ring.conversation_id(),
          maybe_current: ring.current().map(ContentKey::id),
          keys: ring.keys().iter().map(ContentKey::duplicate).collect(),
        }
      },
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

  /// Adds the keys to the ring and switches to the bundle's current key, if any.
  pub fn apply_to(self, ring: &mut ContentKeyRing) -> Result<(), ContentKeyError> {
    for key in self.keys {
      ring.insert(key)?;
    }
    if let Some(current) = self.maybe_current {
      ring.set_current(current)?;
    }
    Ok(())
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
  fn from_join_rotates_and_shares_only_new_key() {
    let mut ring = ContentKeyRing::new(CONVERSATION);
    let old = ring.rotate(&mut OsRng).id();
    let bundle = ContentKeyBundle::for_new_member(&mut ring, HistoryShare::FromJoin, &mut OsRng);

    assert_eq!(bundle.keys().len(), 1);
    assert_ne!(bundle.keys()[0].id(), old);
    assert_eq!(bundle.current(), ring.current().map(ContentKey::id));
  }

  #[test]
  fn full_history_shares_all_keys() {
    let mut ring = ContentKeyRing::new(CONVERSATION);
    ring.rotate(&mut OsRng);
    ring.rotate(&mut OsRng);
    let bundle = ContentKeyBundle::for_new_member(&mut ring, HistoryShare::Full, &mut OsRng);
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
