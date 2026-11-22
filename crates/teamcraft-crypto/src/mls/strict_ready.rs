//! Asking a channel's devices whether they are ready for strict channels, and their signed
//! answers (e2ee.md §4.6.2, "Turning it on", step 2).
//!
//! Before a channel can turn strict, every leaf must announce the capability, and a device gets
//! there only by refreshing its leaf in an empty commit (step 3), which apps from before #1197
//! refuse (`MissingSignedChange`). So first a workspace owner or admin asks ([`StrictAsk`]), and each
//! device of this version answers with a [`SignedStrictReady`]: signed with the key its leaf signs
//! MLS with, so neither the server nor another member can answer for a device that is still on an
//! older app. Only once every leaf that lacks the capability answered does any device refresh. An
//! answer changes nothing in the group, and older apps pass over both entries.

use rand_core::CryptoRngCore;
use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize};

use super::{GroupMember, MlsError};
use crate::domain;
use crate::identity::{DeviceKeyPair, DeviceSigningPublicKey, Ed25519Signature};
use crate::ids::{ConversationId, UserId};
use crate::membership::{ChangeId, MembershipError};
use crate::strict_roles::STRICT_ROLES_FORMAT;

/// The encoding of an ask and an answer this code writes and reads.
const STRICT_READY_VERSION: u8 = 1;
/// The largest encoded ask or answer parsed (both are well under 200 bytes).
const MAX_STRICT_READY_BYTES: usize = 512;

/// "Are your devices ready for strict channels?": what a workspace owner or admin posts into the
/// group's queue. Unsigned: it asks for nothing but answers, and the apps answer only an asker the
/// verified roster names (the caller's to check).
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct StrictAsk {
  version: u8,
  conversation_id: ConversationId,
  /// Random; the answers name it, so one for an earlier ask doesn't count for this one.
  ask_id: ChangeId,
  /// Who asks, as the asker's app claims: checked against the device that posted the ask.
  asker: UserId,
}

/// A device's answer to a [`StrictAsk`]: it runs a version that implements strict channels in
/// `format` (and so takes an empty commit), unsigned.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct StrictReady {
  version: u8,
  conversation_id: ConversationId,
  ask_id: ChangeId,
  /// The answering device, as its leaf names it.
  device: GroupMember,
  /// The strict roles format this device implements ([`STRICT_ROLES_FORMAT`]).
  format: u8,
}

/// A [`StrictReady`] signed with the answering device's key: the key its leaf signs MLS with, which
/// [`super::ConversationGroup::verify_strict_ready`] checks it against.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SignedStrictReady {
  ready: StrictReady,
  signature: Ed25519Signature,
}

impl StrictAsk {
  pub fn new(conversation_id: ConversationId, asker: UserId, rng: &mut impl CryptoRngCore) -> Self {
    Self {
      version: STRICT_READY_VERSION,
      conversation_id,
      ask_id: ChangeId::generate(rng),
      asker,
    }
  }

  /// Parses an ask; one of a newer version or another conversation is refused.
  pub fn from_bytes(bytes: &[u8], conversation_id: ConversationId) -> Result<Self, MlsError> {
    let ask = Self::tls_deserialize_exact_bytes(bounded(bytes)?)?;
    if ask.version != STRICT_READY_VERSION {
      return Err(MembershipError::UnsupportedVersion(ask.version).into());
    }
    if ask.conversation_id != conversation_id {
      return Err(MlsError::WrongConversation {
        expected: conversation_id,
        actual: ask.conversation_id,
      });
    }
    Ok(ask)
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, MlsError> {
    Ok(self.tls_serialize_detached()?)
  }

  pub fn ask_id(&self) -> ChangeId {
    self.ask_id
  }

  /// The asker the ask claims, unverified.
  pub fn asker(&self) -> UserId {
    self.asker
  }

  /// This device's answer, signed with its device key (the key its leaf signs MLS with).
  pub fn answer(&self, device: &DeviceKeyPair) -> SignedStrictReady {
    let ready = StrictReady {
      version: STRICT_READY_VERSION,
      conversation_id: self.conversation_id,
      ask_id: self.ask_id,
      device: GroupMember {
        user_id: device.user_id(),
        device_id: device.device_id(),
      },
      format: STRICT_ROLES_FORMAT,
    };
    let signature = device.sign_labeled(domain::STRICT_READY, &ready.encoded());
    SignedStrictReady { ready, signature }
  }
}

impl StrictReady {
  pub fn ask_id(&self) -> ChangeId {
    self.ask_id
  }

  pub fn device(&self) -> GroupMember {
    self.device
  }

  fn encoded(&self) -> Vec<u8> {
    // Invariant: a struct of fixed-size fields always encodes.
    self.tls_serialize_detached().unwrap_or_default()
  }
}

impl SignedStrictReady {
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, MlsError> {
    Ok(Self::tls_deserialize_exact_bytes(bounded(bytes)?)?)
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, MlsError> {
    Ok(self.tls_serialize_detached()?)
  }

  /// The claimed, unverified answer.
  pub fn unverified(&self) -> &StrictReady {
    &self.ready
  }

  /// Checks the answer against the signature key of the device's leaf (`leaf_signature_key`, as
  /// the tree holds it) and that it answers an ask of `conversation_id`.
  pub(super) fn verify(
    &self,
    conversation_id: ConversationId,
    leaf_signature_key: &[u8],
  ) -> Result<&StrictReady, MlsError> {
    if self.ready.version != STRICT_READY_VERSION {
      return Err(MembershipError::UnsupportedVersion(self.ready.version).into());
    }
    if self.ready.conversation_id != conversation_id {
      return Err(MlsError::WrongConversation {
        expected: conversation_id,
        actual: self.ready.conversation_id,
      });
    }
    if self.ready.format < STRICT_ROLES_FORMAT {
      return Err(MlsError::UnexpectedMessage("a ready for an older strict format"));
    }
    let key = <[u8; 32]>::try_from(leaf_signature_key)
      .map(DeviceSigningPublicKey::from_bytes)
      .map_err(|_| MlsError::UnexpectedMessage("a leaf signature key of the wrong size"))?;
    key
      .verify_labeled(domain::STRICT_READY, &self.ready.encoded(), &self.signature)
      .map_err(|_| MlsError::Membership(MembershipError::InvalidSignature))?;
    Ok(&self.ready)
  }
}

fn bounded(bytes: &[u8]) -> Result<&[u8], MlsError> {
  if bytes.len() > MAX_STRICT_READY_BYTES {
    return Err(MlsError::UnexpectedMessage(
      "a strict ask or ready larger than allowed",
    ));
  }
  Ok(bytes)
}
