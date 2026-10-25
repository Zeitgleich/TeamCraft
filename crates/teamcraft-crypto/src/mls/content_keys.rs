//! Content key bundles: the payload of TeamCraft's MLS application messages (e2ee.md §4.2, §4.3),
//! and bundles sealed to one member device (e2ee.md §4.6, "a device added back").

use hpke_rs::{Hpke, HpkePrivateKey, HpkePublicKey, Mode};
use hpke_rs_crypto::types::{AeadAlgorithm, KdfAlgorithm, KemAlgorithm};
use hpke_rs_rust_crypto::HpkeRustCrypto;
use rand_core::CryptoRngCore;
use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::{GroupMember, MlsError};
use crate::content_key::{ContentKey, ContentKeyError, ContentKeyId, ContentKeyRing, ContentKeyWire};
use crate::domain;
use crate::identity::{DeviceKeyPair, VerifiedDevice};
use crate::ids::ConversationId;
use crate::secret::SecretBytes;

const BUNDLE_VERSION: u8 = 1;
/// First byte of a [`SealedContentKeys`] payload; a plain bundle starts with [`BUNDLE_VERSION`].
const SEALED_VERSION: u8 = 2;
/// Length of the HPKE encapsulated key (X25519) in front of the ciphertext.
const ENCAPSULATED_KEY_LEN: usize = 32;

/// Content keys sent to all group members, optionally marking one as the new current key.
#[derive(Debug)]
pub struct ContentKeyBundle {
  conversation_id: ConversationId,
  maybe_current: Option<ContentKeyId>,
  keys: Vec<ContentKey>,
}

/// Content keys for one member device only, sealed (HPKE, RFC 9180) to the agreement key its
/// certificate names: what a device added back missed while it couldn't follow the group (e2ee.md
/// §4.6). It travels as an MLS application message like a bundle, so only members see it and the
/// sender is authenticated, but the group's other members can't open it: they may not have been
/// members in the epochs the keys come from. Never names a current key.
#[derive(Debug)]
pub struct SealedContentKeys {
  conversation_id: ConversationId,
  recipient: GroupMember,
  /// From the MLS message's authentication, not from the payload.
  sender: GroupMember,
  /// The epoch the MLS message was sent in.
  epoch: u64,
  sealed: Vec<u8>,
}

/// What an MLS application message carries.
pub(super) enum ApplicationPayload {
  Bundle(ContentKeyBundle),
  Sealed(SealedWire),
}

#[derive(TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub(super) struct SealedWire {
  version: u8,
  conversation_id: ConversationId,
  recipient: GroupMember,
  /// Encapsulated key, then the ciphertext of a [`BundleWire`].
  sealed: Vec<u8>,
}

/// What the HPKE info binds a sealed bundle to: a sealed bundle opens only for the device it was
/// sealed for, from the member that sent it, in the group and epoch it was sent in.
#[derive(TlsSerialize, TlsSize)]
struct SealContext {
  conversation_id: ConversationId,
  epoch: u64,
  sender: GroupMember,
  recipient: GroupMember,
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

  /// Every key of the ring, naming `current` (a key of the ring) as the current one: for a sender
  /// that switches to `current` only once the bundle went out.
  pub fn full_history_naming(ring: &ContentKeyRing, current: ContentKeyId) -> Result<Self, ContentKeyError> {
    if ring.get(current).is_none() {
      return Err(ContentKeyError::UnknownKey(current));
    }
    Ok(Self {
      conversation_id: ring.conversation_id(),
      maybe_current: Some(current),
      keys: ring.keys().iter().map(ContentKey::duplicate).collect(),
    })
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

  /// Keys without a current one, for [`SealedContentKeys`]: what a device added back missed, or
  /// the history of a new member (e2ee.md §4.3, §4.6). The current key travels on its own.
  pub fn without_current(conversation_id: ConversationId, keys: Vec<ContentKey>) -> Self {
    Self {
      conversation_id,
      maybe_current: None,
      keys,
    }
  }

  /// Seals the bundle to `recipient`'s certified agreement key (HPKE base mode, X25519, HKDF-SHA256,
  /// ChaCha20-Poly1305), bound to the conversation, the epoch, the sender and the recipient.
  pub(super) fn seal_for(
    &self,
    recipient: &VerifiedDevice,
    sender: GroupMember,
    epoch: u64,
  ) -> Result<Vec<u8>, MlsError> {
    if self.maybe_current.is_some() {
      return Err(MlsError::UnexpectedMessage(
        "sealed content keys never name a current key",
      ));
    }
    let recipient_member = GroupMember::from(recipient);
    let info = seal_info(self.conversation_id, epoch, sender, recipient_member)?;
    let plaintext = self.to_bytes()?;
    let (mut sealed, ciphertext) = hpke()
      .seal(
        &HpkePublicKey::new(recipient.agreement_key().as_bytes().to_vec()),
        &info,
        &[],
        plaintext.expose_secret(),
        None,
        None,
        None,
      )
      .map_err(|error| MlsError::Hpke(format!("{error:?}")))?;
    sealed.extend_from_slice(&ciphertext);
    let wire = SealedWire {
      version: SEALED_VERSION,
      conversation_id: self.conversation_id,
      recipient: recipient_member,
      sealed,
    };
    Ok(wire.tls_serialize_detached()?)
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

impl ApplicationPayload {
  pub(super) fn from_bytes(bytes: &[u8]) -> Result<Self, MlsError> {
    match bytes.first() {
      Some(&SEALED_VERSION) => Ok(Self::Sealed(SealedWire::tls_deserialize_exact_bytes(bytes)?)),
      _ => Ok(Self::Bundle(ContentKeyBundle::from_bytes(bytes)?)),
    }
  }
}

impl SealedContentKeys {
  /// A sealed bundle as received: `sender` and `epoch` from the MLS message that carried it.
  pub(super) fn received(wire: SealedWire, sender: GroupMember, epoch: u64) -> Self {
    Self {
      conversation_id: wire.conversation_id,
      recipient: wire.recipient,
      sender,
      epoch,
      sealed: wire.sealed,
    }
  }

  pub fn conversation_id(&self) -> ConversationId {
    self.conversation_id
  }

  /// The device it is for; the other members ignore it.
  pub fn recipient(&self) -> GroupMember {
    self.recipient
  }

  /// Opens the keys on the device they were sealed for. The bundle names no current key.
  pub fn open(self, device: &DeviceKeyPair) -> Result<ContentKeyBundle, MlsError> {
    let own = GroupMember {
      user_id: device.user_id(),
      device_id: device.public_keys().device_id,
    };
    if self.recipient != own {
      return Err(MlsError::SealedKeysRejected);
    }
    let info = seal_info(self.conversation_id, self.epoch, self.sender, own)?;
    if self.sealed.len() <= ENCAPSULATED_KEY_LEN {
      return Err(MlsError::SealedKeysRejected);
    }
    let (encapsulated, ciphertext) = self.sealed.split_at(ENCAPSULATED_KEY_LEN);
    let (_, agreement) = device.to_secrets();
    let opened = Zeroizing::new(
      hpke()
        .open(
          encapsulated,
          &HpkePrivateKey::new(agreement.expose_secret().to_vec()),
          &info,
          &[],
          ciphertext,
          None,
          None,
          None,
        )
        .map_err(|_| MlsError::SealedKeysRejected)?,
    );
    let bundle = ContentKeyBundle::from_bytes(&opened)?;
    if bundle.conversation_id != self.conversation_id || bundle.maybe_current.is_some() {
      return Err(MlsError::SealedKeysRejected);
    }
    Ok(bundle)
  }
}

impl std::fmt::Debug for BundleWire {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("BundleWire")
      .field("conversation_id", &self.conversation_id)
      .finish_non_exhaustive()
  }
}

fn hpke() -> Hpke<HpkeRustCrypto> {
  Hpke::new(
    Mode::Base,
    KemAlgorithm::DhKem25519,
    KdfAlgorithm::HkdfSha256,
    AeadAlgorithm::ChaCha20Poly1305,
  )
}

fn seal_info(
  conversation_id: ConversationId,
  epoch: u64,
  sender: GroupMember,
  recipient: GroupMember,
) -> Result<Vec<u8>, MlsError> {
  let context = SealContext {
    conversation_id,
    epoch,
    sender,
    recipient,
  };
  Ok(domain::labeled(
    domain::SEALED_CONTENT_KEYS,
    &context.tls_serialize_detached()?,
  ))
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
  fn a_full_history_bundle_names_only_a_key_of_the_ring() {
    let mut ring = ContentKeyRing::new(CONVERSATION);
    let current = ring.rotate(&mut OsRng).id();
    let next = ContentKey::generate(&mut OsRng);
    let next_id = next.id();
    ring.insert(next).unwrap();
    let bundle = ContentKeyBundle::full_history_naming(&ring, next_id).unwrap();
    assert_eq!(bundle.current(), Some(next_id));
    assert_eq!(bundle.keys().len(), 2);
    assert_eq!(
      ring.current().map(ContentKey::id),
      Some(current),
      "the ring is unchanged"
    );
    assert!(matches!(
      ContentKeyBundle::full_history_naming(&ring, ContentKey::generate(&mut OsRng).id()),
      Err(ContentKeyError::UnknownKey(_))
    ));
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
