//! The channel anchor (e2ee.md §4.6.5, issue #1725): who founded a channel or meeting chat, and
//! its space and name, signed by the founder's device and carried in the MLS group context.
//!
//! The founder's device signs a [`ChannelAnchor`] and founds the MLS group with it as the group
//! context extension [`CHANNEL_ANCHOR_EXTENSION_TYPE`]. Every member of an epoch holds the same
//! group context (it goes into the key schedule), so the server can't show two anchors. Moves and
//! renames replace the anchor's properties with a signed [`SignedPropertiesChange`].
//!
//! One rule runs through all of it: **the anchor only grants.** A device that can't verify an
//! anchor judges as it did before anchors existed; it never refuses more than that.
//!
//! This module is the pure part: the encoding, the labelled hashes, signing, the verdicts and the
//! per-conversation [`AnchorFloor`]. Where the anchor sits in the group, and which commits may change
//! it, is [`crate::mls::ConversationGroup`]'s.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use tls_codec::{
  DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize, VLByteSlice, VLBytes,
};

use crate::domain;
use crate::identity::{DeviceKeyPair, IdentityPublicKey, SignedDeviceCertificate, VerifiedDevice};
use crate::ids::{ConversationId, MeetingId, SpaceId, UserId};
use crate::membership::{IdentityDirectory, MembershipAuthority};
use crate::workspace::{WorkspaceLog, WorkspaceLogError, claimed_signer, sign_log_entry, verify_log_entry};

/// The private-use group context extension type of the anchor (RFC 9420 §17.3). TeamCraft's are
/// `0xF7C0` (§4.6.2's strict roles) and this one; a new one takes the next number.
pub const CHANNEL_ANCHOR_EXTENSION_TYPE: u16 = 0xF7C1;
/// The anchor encoding this code writes and reads. A newer one grants nothing: the app has to update.
pub const ANCHOR_FORMAT: u8 = 1;
/// Default of the client setting `max_anchor_bytes`: a larger extension isn't parsed.
pub const DEFAULT_MAX_ANCHOR_BYTES: usize = 4 * 1024;
/// How many epochs after the anchoring a device judges the signer on the newest device list and
/// roster it holds, and how many epochs a signed properties change stays valid (§4.6, "Freshness").
/// A constant, not a setting: every member must decide alike, or the group splits.
pub const ANCHORING_WINDOW_EPOCHS: u64 = 32;
/// Marks a commit's authenticated data as a [`SignedPropertiesChange`].
pub const PROPERTIES_CHANGE_AAD_MAGIC: &[u8] = b"TCAP";

/// The anchor as it sits in the group context (e2ee.md §4.6.5, "What is signed").
///
/// The founding and its first properties stay byte for byte for ever; the founding entry signs
/// both. The last move or rename, if any, carries its own signature, so the extension verifies on
/// its own: a device that only ever saw the current epoch checks the founding and the last change
/// without the history of commits.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct ChannelAnchor {
  format: u8,
  founding: ChannelAnchorFounding,
  /// Version 1 of the properties, as the founding signed them.
  founding_properties: ChannelProperties,
  /// A workspace log entry signature (kind [`WorkspaceLog::ChannelAnchor`]) over the founding and
  /// `founding_properties`.
  founding_entry: VLBytes,
  /// The change that set the current properties; none while they are the founding's.
  maybe_last_change: Option<SignedPropertiesChange>,
}

/// Who founded the group, for which conversation, and when.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct ChannelAnchorFounding {
  /// Equals the MLS group's conversation (the group ID is derived from it).
  pub conversation_id: ConversationId,
  pub kind: AnchorKind,
  pub origin: AnchorOrigin,
  /// The signer's signed device list that names the signing device.
  pub device_list_version: u64,
  /// The signer's clock: shown, never decided on.
  pub signed_at_ms: u64,
}

/// What the anchored group is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
#[repr(u8)]
pub enum AnchorKind {
  #[tls_codec(discriminant = 1)]
  Channel,
  /// A meeting's chat; its founder is the organizer (D10).
  #[tls_codec(discriminant = 2)]
  MeetingChat { meeting_id: MeetingId },
}

/// How the anchor came to be. Only `Founded` is signed by this release; the other two are read so
/// that a newer device's anchor of a reviewed (#1728) or re-founded channel verifies here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
#[repr(u8)]
pub enum AnchorOrigin {
  /// The group was founded with it.
  #[tls_codec(discriminant = 1)]
  Founded {
    founder_user_id: UserId,
    founder_identity_key: IdentityPublicKey,
  },
  /// A workspace owner or admin anchored a channel from before anchors (#1728).
  #[tls_codec(discriminant = 2)]
  Reviewed {
    reviewer_user_id: UserId,
    reviewer_identity_key: IdentityPublicKey,
    roster_version: u64,
    signed_for_epoch: u64,
    role_log_head: AnchorHash,
  },
  /// A workspace owner or admin founded a lost group again.
  #[tls_codec(discriminant = 3)]
  Refounded {
    signer_user_id: UserId,
    signer_identity_key: IdentityPublicKey,
    roster_version: u64,
    replaces_founding: AnchorHash,
    replaces_last_epoch: u64,
  },
}

/// What a channel is called and where it sits, as signed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct ChannelProperties {
  /// 1 at the founding, one up per change.
  pub version: u64,
  /// `None`: a channel outside any space.
  pub maybe_space_id: Option<SpaceId>,
  /// [`name_hash`] of the name.
  pub name_hash: AnchorHash,
  /// Who signed the current name.
  pub name_set_by: NameSetter,
  /// Zero at version 1; the [`ChannelProperties::hash`] of the properties this version replaced.
  pub prev_hash: AnchorHash,
}

/// Who signed a channel's current name, and in which role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct NameSetter {
  pub authority: NameAuthority,
  pub user_id: UserId,
}

/// The role a name was signed in. Only a member's name (`Member`) is stale for deletions (D5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, TlsSerialize, TlsDeserializeBytes, TlsSize)]
#[repr(u8)]
pub enum NameAuthority {
  Founder = 1,
  /// An admin of a strict channel (§4.6.2); signed once #1197's extension exists.
  ChannelAdmin = 2,
  /// A workspace owner or admin of a fresh roster.
  Workspace = 3,
  /// Any member with a leaf in the group.
  Member = 4,
}

/// A move or rename, the payload of a [`WorkspaceLog::ChannelProperties`] entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct ChannelPropertiesChange {
  pub conversation_id: ConversationId,
  /// The anchor's founding hash: a change counts under one founding only.
  pub founding_hash: AnchorHash,
  /// Accepted up to [`ANCHORING_WINDOW_EPOCHS`] later.
  pub signed_for_epoch: u64,
  /// The new properties.
  pub properties: ChannelProperties,
  /// For a workspace signer: the roster that made them one; 0 otherwise.
  pub roster_version: u64,
}

/// A properties change with its signature: the anchor's last change, and the authenticated data
/// of the commit that makes it (after [`PROPERTIES_CHANGE_AAD_MAGIC`]).
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SignedPropertiesChange {
  change: ChannelPropertiesChange,
  /// A workspace log entry signature over `change`.
  entry: VLBytes,
}

/// A SHA-256 value in the anchor: a name hash, a properties hash or a founding hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct AnchorHash([u8; 32]);

/// What a device expects of the anchor of a group it is in or joins (check 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnchorExpectation {
  pub conversation_id: ConversationId,
  pub kind: ExpectedKind,
}

/// The kind of conversation the server lists, as far as the device can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedKind {
  Channel,
  /// A meeting's chat: the anchor must name the meeting and the organizer the server names as
  /// its founder (D10).
  MeetingChat {
    meeting_id: MeetingId,
    organizer: UserId,
  },
}

/// Which device list or roster a check reads (e2ee.md §4.6.5, "Judged as of the anchoring").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Judged {
  /// Within the anchoring window: the newest the device holds; a newer one that revokes or demotes
  /// refuses.
  Newest,
  /// After the window: only the version the anchor names.
  AsNamed,
}

/// An answer about a signer from the device's signed state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
  Confirmed,
  /// The device can't tell yet (a list or roster it hasn't fetched or can't verify): grants
  /// nothing until fetched again.
  Unknown,
  Refused,
}

/// Whether a property of a verified anchor holds for this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
  Confirmed,
  /// Signed, but the device can't confirm the signer may set it: decisions that rely on it take
  /// the retention.
  Stale,
}

/// An anchor whose founding verified (checks 1 to 5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedAnchor {
  anchor: ChannelAnchor,
  founding_hash: AnchorHash,
  properties_hash: AnchorHash,
  space: Confidence,
  name: Confidence,
}

/// What a device records per conversation (e2ee.md §4.6.5, "The pin and the floor"), kept in the
/// account data next to the group epoch floor and merged by maximum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorFloor {
  pub founding_hash: AnchorHash,
  pub properties_version: u64,
  pub properties_hash: AnchorHash,
  /// The highest epoch in which this user's devices saw the anchor.
  pub epoch: u64,
  #[serde(default, skip_serializing_if = "FloorState::is_agreed")]
  pub state: FloorState,
}

/// Whether the user's devices saw one history of the anchor or two.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FloorState {
  #[default]
  Agreed,
  /// Two hashes for one properties version: settled by a higher version.
  ForkedProperties,
  /// Two foundings no re-founding connects: settled only by a re-founding.
  ForkedFounding,
  /// A state a newer app wrote: grants nothing, and no higher version settles it here.
  #[serde(other)]
  Unknown,
}

/// What a founding device signs (e2ee.md §4.6.5, "Founding"): what it asked the server for, never
/// the server's answer.
#[derive(Clone, Copy, Debug)]
pub struct Founding<'a> {
  pub conversation_id: ConversationId,
  pub kind: AnchorKind,
  /// The newest signed device list of the founder that names the founding device.
  pub device_list_version: u64,
  pub signed_at_ms: u64,
  pub maybe_space_id: Option<SpaceId>,
  /// The name exactly as sent in the create request (UTF-8, already checked at the field).
  pub name: &'a str,
}

/// The properties a move or rename sets: the full new values.
#[derive(Clone, Copy, Debug)]
pub struct PropertiesTarget<'a> {
  pub maybe_space_id: Option<SpaceId>,
  pub name: &'a str,
}

/// Why an anchor grants nothing on this device. The group isn't left for it.
#[derive(Debug, thiserror::Error)]
pub enum AnchorError {
  #[error("anchor of {size} bytes is larger than {max}")]
  TooLarge { size: usize, max: usize },
  /// A newer format, kind or origin: the app has to update.
  #[error("anchor in a newer format; the app has to update")]
  MustUpdate,
  #[error("anchor doesn't parse: {0}")]
  Malformed(tls_codec::Error),
  #[error("anchor names conversation {actual}, expected {expected}")]
  WrongConversation {
    expected: ConversationId,
    actual: ConversationId,
  },
  #[error("anchor is of another kind than the conversation")]
  WrongKind,
  #[error("meeting chat's anchor doesn't name the organizer as founder")]
  NotTheOrganizer,
  #[error("a pinned identity key of {0} differs from the one the founding names")]
  PinnedKeyDiffers(UserId),
  #[error("founding signature: {0}")]
  FoundingSignature(WorkspaceLogError),
  /// The signing device isn't on the signer's signed device list as far as this device knows:
  /// fetch the list again.
  #[error("founding device isn't on its user's device list yet")]
  DeviceNotListed,
  #[error("founding device was revoked")]
  DeviceRevoked,
  /// The roster that makes the signer a workspace owner or admin isn't verified here yet.
  #[error("roster version {0} can't be verified yet")]
  RosterUnverifiable(u64),
  #[error("signer {0} isn't a workspace owner or admin")]
  SignerNotAdmin(UserId),
  #[error("review signed for epoch {signed_for_epoch}, after epoch {epoch}")]
  ReviewFromTheFuture { signed_for_epoch: u64, epoch: u64 },
  #[error("founding properties aren't version 1 by the founder")]
  FoundingProperties,
  #[error("signing failed: {0}")]
  Signing(WorkspaceLogError),
  #[error("encoding failed: {0}")]
  Encoding(tls_codec::Error),
}

/// Why a move or rename commit is refused (e2ee.md §4.6.5, "Moves and renames").
#[derive(Debug, thiserror::Error)]
pub enum PropertiesChangeError {
  #[error("change for conversation {0}, not this one")]
  WrongConversation(ConversationId),
  #[error("change for another founding")]
  WrongFounding,
  #[error("change to version {offered}, expected {expected}")]
  WrongVersion { expected: u64, offered: u64 },
  #[error("change doesn't continue the current properties")]
  BrokenChain,
  #[error("change signed in epoch {signed_epoch} is stale in epoch {current_epoch}")]
  Stale { signed_epoch: u64, current_epoch: u64 },
  #[error("change signature: {0}")]
  Signature(WorkspaceLogError),
  #[error("{0} may not make this change")]
  NotAllowed(UserId),
  /// The signing device isn't on its user's newest signed device list (or was revoked).
  #[error("the device of {0} that signed the change isn't on its signed device list")]
  DeviceNotListed(UserId),
  #[error("the name's setter doesn't match the signer")]
  WrongNameSetter,
}

impl ChannelAnchor {
  /// Signs a new group's anchor with the founding device. The founder's identity key is the one
  /// the device certificate names; the name setter is the founder.
  pub fn found(
    device: &DeviceKeyPair,
    certificate: &SignedDeviceCertificate,
    founding: &Founding<'_>,
  ) -> Result<Self, AnchorError> {
    let signer = certificate.unverified();
    let anchor_founding = ChannelAnchorFounding {
      conversation_id: founding.conversation_id,
      kind: founding.kind,
      origin: AnchorOrigin::Founded {
        founder_user_id: signer.user_id,
        founder_identity_key: signer.identity_key,
      },
      device_list_version: founding.device_list_version,
      signed_at_ms: founding.signed_at_ms,
    };
    let properties = ChannelProperties {
      version: 1,
      maybe_space_id: founding.maybe_space_id,
      name_hash: name_hash(founding.name),
      name_set_by: NameSetter {
        authority: NameAuthority::Founder,
        user_id: signer.user_id,
      },
      prev_hash: AnchorHash::ZERO,
    };
    let payload = founding_payload(&anchor_founding, &properties)?;
    let entry = sign_log_entry(
      device,
      certificate,
      WorkspaceLog::ChannelAnchor,
      u64::from(ANCHOR_FORMAT),
      &payload,
    )
    .map_err(AnchorError::Signing)?;
    Ok(Self {
      format: ANCHOR_FORMAT,
      founding: anchor_founding,
      founding_properties: properties,
      founding_entry: VLBytes::new(entry),
      maybe_last_change: None,
    })
  }

  /// Parses the extension's content: within `max_bytes`, completely, in a known format.
  pub fn from_bytes(bytes: &[u8], max_bytes: usize) -> Result<Self, AnchorError> {
    if bytes.len() > max_bytes {
      return Err(AnchorError::TooLarge {
        size: bytes.len(),
        max: max_bytes,
      });
    }
    match bytes.first() {
      None => return Err(AnchorError::Malformed(tls_codec::Error::EndOfStream)),
      Some(&format) if format != ANCHOR_FORMAT => return Err(AnchorError::MustUpdate),
      Some(_) => {},
    }
    Self::tls_deserialize_exact_bytes(bytes).map_err(|error| match error {
      tls_codec::Error::UnknownValue(_) if has_newer_kind_or_origin(bytes) => AnchorError::MustUpdate,
      other => AnchorError::Malformed(other),
    })
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, AnchorError> {
    self.tls_serialize_detached().map_err(AnchorError::Encoding)
  }

  pub fn founding(&self) -> &ChannelAnchorFounding {
    &self.founding
  }

  /// The current properties: the last change's, or the founding's.
  pub fn properties(&self) -> &ChannelProperties {
    self
      .maybe_last_change
      .as_ref()
      .map_or(&self.founding_properties, |signed| &signed.change.properties)
  }

  pub fn last_change(&self) -> Option<&SignedPropertiesChange> {
    self.maybe_last_change.as_ref()
  }

  /// The pin devices keep: over the founding and its first properties, without the signature, so
  /// a re-encoded or re-signed founding of the same content has the same pin.
  pub fn founding_hash(&self) -> Result<AnchorHash, AnchorError> {
    let payload = founding_payload(&self.founding, &self.founding_properties)?;
    Ok(labeled_hash(domain::CHANNEL_ANCHOR_PIN, &payload))
  }

  /// The anchor with `signed` as its last change, everything else byte for byte.
  pub fn with_change(&self, signed: SignedPropertiesChange) -> Self {
    Self {
      maybe_last_change: Some(signed),
      ..self.clone()
    }
  }

  /// Signs a move or rename of this anchor's channel by the device's user in the role `class`
  /// (the verdict a member device gives them, [`Self::check_change`]). The name's setter changes
  /// only if the name does.
  pub fn sign_change(
    &self,
    device: &DeviceKeyPair,
    certificate: &SignedDeviceCertificate,
    class: NameAuthority,
    target: &PropertiesTarget<'_>,
    signed_for_epoch: u64,
    roster_version: u64,
  ) -> Result<SignedPropertiesChange, AnchorError> {
    let current = self.properties();
    let new_name_hash = name_hash(target.name);
    let name_set_by = if new_name_hash == current.name_hash {
      current.name_set_by
    } else {
      NameSetter {
        authority: class,
        user_id: certificate.unverified().user_id,
      }
    };
    let change = ChannelPropertiesChange {
      conversation_id: self.founding.conversation_id,
      founding_hash: self.founding_hash()?,
      signed_for_epoch,
      properties: ChannelProperties {
        version: current.version.saturating_add(1),
        maybe_space_id: target.maybe_space_id,
        name_hash: new_name_hash,
        name_set_by,
        prev_hash: current.hash()?,
      },
      roster_version,
    };
    SignedPropertiesChange::sign(device, certificate, change)
  }

  /// Verifies the anchor of a group context this device accepts (checks 1 to 5 of e2ee.md
  /// §4.6.5): at a welcome or add-back, an external commit's group information, a commit that
  /// changes it and the group's creation. `epoch` is the group's epoch; `tree_users` are the users
  /// with a leaf in the group. The pin and the floor (check 6) are [`AnchorFloor`]'s.
  pub fn verify(
    &self,
    expected: &AnchorExpectation,
    epoch: u64,
    tree_users: &BTreeSet<UserId>,
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<VerifiedAnchor, AnchorError> {
    if self.format != ANCHOR_FORMAT {
      return Err(AnchorError::MustUpdate);
    }
    self.check_expectation(expected)?;
    let judged = self.judged_at(epoch);
    self.check_founding_signature(directory, judged)?;
    self.check_workspace_signer(epoch, authority, judged)?;
    self.check_founding_properties()?;
    let founding_hash = self.founding_hash()?;
    let (space, name) = self.last_change_confidence(founding_hash, epoch, tree_users, directory, authority);
    Ok(VerifiedAnchor {
      properties_hash: self.properties().hash()?,
      anchor: self.clone(),
      founding_hash,
      space,
      name,
    })
  }

  /// The verdict on a move or rename committed to the group in `epoch` (e2ee.md §4.6.5, "Moves
  /// and renames"): it continues this anchor's properties, is fresh, and its signer may make it.
  /// Reads only the anchor, the tree (`tree_users`) and the roster, so every member decides alike.
  /// Returns the signer's role.
  pub fn check_change(
    &self,
    signed: &SignedPropertiesChange,
    epoch: u64,
    tree_users: &BTreeSet<UserId>,
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<NameAuthority, PropertiesChangeError> {
    let change = &signed.change;
    let current = self.properties();
    if change.conversation_id != self.founding.conversation_id {
      return Err(PropertiesChangeError::WrongConversation(change.conversation_id));
    }
    let founding_hash = self
      .founding_hash()
      .map_err(|_| PropertiesChangeError::WrongFounding)?;
    if change.founding_hash != founding_hash {
      return Err(PropertiesChangeError::WrongFounding);
    }
    let expected = current.version.saturating_add(1);
    if change.properties.version != expected {
      return Err(PropertiesChangeError::WrongVersion {
        expected,
        offered: change.properties.version,
      });
    }
    let is_chained = current
      .hash()
      .is_ok_and(|hash| hash == change.properties.prev_hash);
    if !is_chained {
      return Err(PropertiesChangeError::BrokenChain);
    }
    if change.signed_for_epoch > epoch || epoch - change.signed_for_epoch > ANCHORING_WINDOW_EPOCHS {
      return Err(PropertiesChangeError::Stale {
        signed_epoch: change.signed_for_epoch,
        current_epoch: epoch,
      });
    }
    let device = signed
      .verify_signature(directory)
      .map_err(PropertiesChangeError::Signature)?;
    let signer = device.user_id();
    if !is_listed_signer(&device, directory) {
      return Err(PropertiesChangeError::DeviceNotListed(signer));
    }
    let class = signer_class(
      signer,
      change.roster_version,
      Judged::Newest,
      tree_users,
      directory,
      authority,
    )
    .ok_or(PropertiesChangeError::NotAllowed(signer))?;
    let is_move = change.properties.maybe_space_id != current.maybe_space_id;
    if is_move && class != NameAuthority::Workspace {
      return Err(PropertiesChangeError::NotAllowed(signer));
    }
    let expected_setter = if change.properties.name_hash == current.name_hash {
      current.name_set_by
    } else {
      NameSetter {
        authority: class,
        user_id: signer,
      }
    };
    if change.properties.name_set_by != expected_setter {
      return Err(PropertiesChangeError::WrongNameSetter);
    }
    Ok(class)
  }

  fn check_expectation(&self, expected: &AnchorExpectation) -> Result<(), AnchorError> {
    if self.founding.conversation_id != expected.conversation_id {
      return Err(AnchorError::WrongConversation {
        expected: expected.conversation_id,
        actual: self.founding.conversation_id,
      });
    }
    match (expected.kind, self.founding.kind) {
      (ExpectedKind::Channel, AnchorKind::Channel) => Ok(()),
      (
        ExpectedKind::MeetingChat {
          meeting_id,
          organizer,
        },
        AnchorKind::MeetingChat { meeting_id: anchored },
      ) => {
        if meeting_id != anchored {
          return Err(AnchorError::WrongKind);
        }
        match self.founding.origin {
          AnchorOrigin::Founded { founder_user_id, .. } if founder_user_id == organizer => Ok(()),
          _ => Err(AnchorError::NotTheOrganizer),
        }
      },
      _ => Err(AnchorError::WrongKind),
    }
  }

  /// The epoch the anchor was set in: 0 for a founding or re-founding, the reviewed epoch for a
  /// review.
  fn anchoring_epoch(&self) -> u64 {
    match self.founding.origin {
      AnchorOrigin::Reviewed { signed_for_epoch, .. } => signed_for_epoch,
      AnchorOrigin::Founded { .. } | AnchorOrigin::Refounded { .. } => 0,
    }
  }

  fn judged_at(&self, epoch: u64) -> Judged {
    if epoch <= self.anchoring_epoch().saturating_add(ANCHORING_WINDOW_EPOCHS) {
      Judged::Newest
    } else {
      Judged::AsNamed
    }
  }

  /// Check 3: the founding entry verifies under the identity key the founding names, which no pin
  /// of this device contradicts, and the signing device is on its user's signed device list.
  fn check_founding_signature(
    &self,
    directory: &impl IdentityDirectory,
    judged: Judged,
  ) -> Result<(), AnchorError> {
    let (user_id, key) = self.founding.origin.signer();
    if directory
      .pinned_identity(user_id)
      .is_some_and(|pinned| pinned != key)
    {
      return Err(AnchorError::PinnedKeyDiffers(user_id));
    }
    let payload = founding_payload(&self.founding, &self.founding_properties)?;
    let device = verify_log_entry(
      self.founding_entry.as_slice(),
      WorkspaceLog::ChannelAnchor,
      u64::from(self.format),
      &payload,
      &OneKey { user_id, key },
    )
    .map_err(AnchorError::FoundingSignature)?;
    match directory.device_listing(&device, self.founding.device_list_version, judged) {
      Standing::Confirmed => Ok(()),
      Standing::Unknown => Err(AnchorError::DeviceNotListed),
      Standing::Refused => Err(AnchorError::DeviceRevoked),
    }
  }

  /// Check 4: a review or re-founding is signed by a workspace owner or admin.
  fn check_workspace_signer(
    &self,
    epoch: u64,
    authority: &impl MembershipAuthority,
    judged: Judged,
  ) -> Result<(), AnchorError> {
    let (user_id, roster_version) = match self.founding.origin {
      AnchorOrigin::Founded { .. } => return Ok(()),
      AnchorOrigin::Reviewed {
        reviewer_user_id,
        roster_version,
        signed_for_epoch,
        ..
      } => {
        if signed_for_epoch > epoch {
          return Err(AnchorError::ReviewFromTheFuture {
            signed_for_epoch,
            epoch,
          });
        }
        (reviewer_user_id, roster_version)
      },
      AnchorOrigin::Refounded {
        signer_user_id,
        roster_version,
        ..
      } => (signer_user_id, roster_version),
    };
    match authority.workspace_signer(user_id, roster_version, judged) {
      Standing::Confirmed => Ok(()),
      Standing::Unknown => Err(AnchorError::RosterUnverifiable(roster_version)),
      Standing::Refused => Err(AnchorError::SignerNotAdmin(user_id)),
    }
  }

  /// Check 5 for version 1: zero `prev_hash`, the founder (or the workspace signer) set the name.
  fn check_founding_properties(&self) -> Result<(), AnchorError> {
    let properties = &self.founding_properties;
    let (user_id, _) = self.founding.origin.signer();
    let authority = match self.founding.origin {
      AnchorOrigin::Founded { .. } => NameAuthority::Founder,
      AnchorOrigin::Reviewed { .. } | AnchorOrigin::Refounded { .. } => NameAuthority::Workspace,
    };
    let is_valid = properties.version == 1
      && properties.prev_hash == AnchorHash::ZERO
      && properties.name_set_by == NameSetter { authority, user_id };
    if is_valid {
      Ok(())
    } else {
      Err(AnchorError::FoundingProperties)
    }
  }

  /// Check 5 above version 1, on the extension alone: whether this device can confirm the last
  /// change's signer may set its space and name. A signer it can't confirm makes the property
  /// stale for this device, not the anchor invalid.
  fn last_change_confidence(
    &self,
    founding_hash: AnchorHash,
    epoch: u64,
    tree_users: &BTreeSet<UserId>,
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> (Confidence, Confidence) {
    let Some(signed) = &self.maybe_last_change else {
      return (Confidence::Confirmed, Confidence::Confirmed);
    };
    let change = &signed.change;
    let is_ours = change.conversation_id == self.founding.conversation_id
      && change.founding_hash == founding_hash
      && change.properties.version >= 2;
    // The change's own anchoring window (§4.6, "Freshness"): within it, the newest roster the
    // device holds (a demoted signer counts for nothing); later, the roster the change names,
    // which the members of the window checked fresh. A change signed after this epoch is no
    // change this device can place.
    if change.signed_for_epoch > epoch {
      return (Confidence::Stale, Confidence::Stale);
    }
    let judged = if epoch <= change.signed_for_epoch.saturating_add(ANCHORING_WINDOW_EPOCHS) {
      Judged::Newest
    } else {
      Judged::AsNamed
    };
    let maybe_class = signed
      .verify_signature(directory)
      .ok()
      .filter(|device| is_ours && is_listed_signer(device, directory))
      .and_then(|device| {
        let signer = device.user_id();
        signer_class(
          signer,
          change.roster_version,
          judged,
          tree_users,
          directory,
          authority,
        )
        .map(|class| (signer, class))
      });
    match maybe_class {
      None => (Confidence::Stale, Confidence::Stale),
      Some((_, NameAuthority::Workspace)) => (Confidence::Confirmed, Confidence::Confirmed),
      Some((signer, _)) => {
        // A member renames, never moves: a space other than the founding's needs a workspace
        // signer, and the name must be theirs. The history in between isn't visible here.
        let space = if change.properties.maybe_space_id == self.founding_properties.maybe_space_id {
          Confidence::Confirmed
        } else {
          Confidence::Stale
        };
        let own_name = NameSetter {
          authority: NameAuthority::Member,
          user_id: signer,
        };
        let name = if change.properties.name_set_by == own_name {
          Confidence::Confirmed
        } else {
          Confidence::Stale
        };
        (space, name)
      },
    }
  }
}

impl AnchorOrigin {
  /// The user who signed the founding entry, with the identity key the founding names.
  pub fn signer(&self) -> (UserId, IdentityPublicKey) {
    match *self {
      Self::Founded {
        founder_user_id,
        founder_identity_key,
      } => (founder_user_id, founder_identity_key),
      Self::Reviewed {
        reviewer_user_id,
        reviewer_identity_key,
        ..
      } => (reviewer_user_id, reviewer_identity_key),
      Self::Refounded {
        signer_user_id,
        signer_identity_key,
        ..
      } => (signer_user_id, signer_identity_key),
    }
  }
}

impl ChannelProperties {
  /// The labelled hash a later version's `prev_hash` holds.
  pub fn hash(&self) -> Result<AnchorHash, AnchorError> {
    let encoded = self.tls_serialize_detached().map_err(AnchorError::Encoding)?;
    Ok(labeled_hash(domain::CHANNEL_PROPERTIES, &encoded))
  }
}

impl SignedPropertiesChange {
  pub fn sign(
    device: &DeviceKeyPair,
    certificate: &SignedDeviceCertificate,
    change: ChannelPropertiesChange,
  ) -> Result<Self, AnchorError> {
    let payload = change.tls_serialize_detached().map_err(AnchorError::Encoding)?;
    let entry = sign_log_entry(
      device,
      certificate,
      WorkspaceLog::ChannelProperties,
      change.properties.version,
      &payload,
    )
    .map_err(AnchorError::Signing)?;
    Ok(Self {
      change,
      entry: VLBytes::new(entry),
    })
  }

  pub fn from_bytes(bytes: &[u8]) -> Result<Self, AnchorError> {
    Self::tls_deserialize_exact_bytes(bytes).map_err(AnchorError::Malformed)
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, AnchorError> {
    self.tls_serialize_detached().map_err(AnchorError::Encoding)
  }

  pub fn change(&self) -> &ChannelPropertiesChange {
    &self.change
  }

  /// The user the entry claims as signer, unverified: whose identity key to pin first.
  pub fn claimed_signer(&self) -> Result<UserId, WorkspaceLogError> {
    claimed_signer(self.entry.as_slice())
  }

  /// Verifies the signature under the signer's pinned identity key; returns the signing device.
  fn verify_signature(
    &self,
    directory: &impl IdentityDirectory,
  ) -> Result<VerifiedDevice, WorkspaceLogError> {
    let payload = self.change.tls_serialize_detached()?;
    verify_log_entry(
      self.entry.as_slice(),
      WorkspaceLog::ChannelProperties,
      self.change.properties.version,
      &payload,
      directory,
    )
  }
}

impl AnchorHash {
  pub const ZERO: Self = Self([0; 32]);

  pub const fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
  }

  pub const fn as_bytes(&self) -> &[u8; 32] {
    &self.0
  }

  fn to_hex(self) -> String {
    self.0.iter().map(|byte| format!("{byte:02x}")).collect()
  }

  fn from_hex(text: &str) -> Option<Self> {
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
      return None;
    }
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
      *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(Self(bytes))
  }
}

impl fmt::Debug for AnchorHash {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "AnchorHash({})", self.to_hex())
  }
}

impl Serialize for AnchorHash {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&self.to_hex())
  }
}

impl<'de> Deserialize<'de> for AnchorHash {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let text = String::deserialize(deserializer)?;
    Self::from_hex(&text).ok_or_else(|| serde::de::Error::custom("expected 64 hex digits"))
  }
}

impl VerifiedAnchor {
  pub fn anchor(&self) -> &ChannelAnchor {
    &self.anchor
  }

  pub fn founding_hash(&self) -> AnchorHash {
    self.founding_hash
  }

  pub fn properties(&self) -> &ChannelProperties {
    self.anchor.properties()
  }

  pub fn properties_hash(&self) -> AnchorHash {
    self.properties_hash
  }

  /// The founder of a `Founded` anchor; `None` for a review or re-founding, whose role log head
  /// is the signer's list.
  pub fn founder(&self) -> Option<(UserId, IdentityPublicKey)> {
    match self.anchor.founding.origin {
      AnchorOrigin::Founded { .. } => Some(self.anchor.founding.origin.signer()),
      AnchorOrigin::Reviewed { .. } | AnchorOrigin::Refounded { .. } => None,
    }
  }

  /// Whether the anchor's space counts for this device (a space's deletion, #1727).
  pub fn space_confidence(&self) -> Confidence {
    self.space
  }

  /// Whether the anchor's name counts for this device for any purpose (what the channel is
  /// called). For a deletion use [`Self::name_counts_for_deletion`].
  pub fn name_confidence(&self) -> Confidence {
    self.name
  }

  /// Whether a deletion naming `name_hash` may purge at once as far as the name goes (D3, D5):
  /// it is the anchor's name, confirmed, and not set by a member.
  pub fn name_counts_for_deletion(&self, deletion_name_hash: Option<AnchorHash>) -> bool {
    let properties = self.properties();
    self.name == Confidence::Confirmed
      && properties.name_set_by.authority != NameAuthority::Member
      && deletion_name_hash == Some(properties.name_hash)
  }
}

impl AnchorFloor {
  /// The first anchor this user's devices accept for the conversation, seen in `epoch`.
  pub fn first(anchor: &VerifiedAnchor, epoch: u64) -> Self {
    Self {
      founding_hash: anchor.founding_hash,
      properties_version: anchor.properties().version,
      properties_hash: anchor.properties_hash,
      epoch,
      state: FloorState::Agreed,
    }
  }

  /// Check 6 when the device **enters** a group state (a welcome, an add-back, an external commit):
  /// the pinned founding, at or above the floor's properties version (the same version only under
  /// the same hash), and an epoch not below the floor's.
  pub fn check_entering(&self, anchor: &VerifiedAnchor, epoch: u64) -> Result<(), FloorError> {
    self.check_inside(anchor)?;
    let version = anchor.properties().version;
    if version < self.properties_version {
      return Err(FloorError::OlderProperties {
        offered: version,
        floor: self.properties_version,
      });
    }
    if version == self.properties_version && anchor.properties_hash != self.properties_hash {
      return Err(FloorError::OtherProperties(version));
    }
    if epoch < self.epoch {
      return Err(FloorError::OlderEpoch {
        offered: epoch,
        floor: self.epoch,
      });
    }
    Ok(())
  }

  /// Check 6 at a commit inside a group state the device already holds: the MLS chain carries the
  /// continuity, so only the founding pin is checked (the floor, merged across devices, may be
  /// ahead of this device's state).
  pub fn check_inside(&self, anchor: &VerifiedAnchor) -> Result<(), FloorError> {
    if anchor.founding_hash == self.founding_hash {
      Ok(())
    } else {
      Err(FloorError::OtherFounding)
    }
  }

  /// Raises the floor to what the device now holds; returns whether it changed. A higher version
  /// settles a fork of properties.
  pub fn raise(&mut self, anchor: &VerifiedAnchor, epoch: u64) -> bool {
    if anchor.founding_hash != self.founding_hash {
      return false;
    }
    self.take(anchor.properties().version, anchor.properties_hash, epoch)
  }

  /// Merges another device's record into this one by maximum; returns whether this changed. Two
  /// foundings, or two hashes for one version, are a fork the user's devices saw both sides of.
  pub fn merge(&mut self, other: &Self) -> bool {
    let before = *self;
    if other.founding_hash != self.founding_hash {
      self.state = FloorState::ForkedFounding;
      return *self != before;
    }
    if matches!(other.state, FloorState::ForkedFounding | FloorState::Unknown) {
      self.state = self.state.max_with(other.state);
    }
    self.take(other.properties_version, other.properties_hash, other.epoch);
    // The other side's fork of properties stands while its version is still the highest.
    if other.state == FloorState::ForkedProperties && other.properties_version == self.properties_version {
      self.state = self.state.max_with(FloorState::ForkedProperties);
    }
    *self != before
  }

  /// Whether the anchor may grant anything in this conversation.
  pub fn grants(&self) -> bool {
    self.state == FloorState::Agreed
  }

  fn take(&mut self, version: u64, hash: AnchorHash, epoch: u64) -> bool {
    let before = *self;
    if version > self.properties_version {
      self.properties_version = version;
      self.properties_hash = hash;
      if self.state == FloorState::ForkedProperties {
        self.state = FloorState::Agreed;
      }
    } else if version == self.properties_version && hash != self.properties_hash {
      self.state = self.state.max_with(FloorState::ForkedProperties);
    }
    self.epoch = self.epoch.max(epoch);
    *self != before
  }
}

impl FloorState {
  fn is_agreed(&self) -> bool {
    *self == Self::Agreed
  }

  /// The more severe of the two.
  fn max_with(self, other: Self) -> Self {
    match (self, other) {
      (Self::ForkedFounding, _) | (_, Self::ForkedFounding) => Self::ForkedFounding,
      (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
      (Self::ForkedProperties, _) | (_, Self::ForkedProperties) => Self::ForkedProperties,
      _ => Self::Agreed,
    }
  }
}

/// Why a group state is refused against the pin and the floor (e2ee.md §4.6.5, "Stripping and
/// replacing"): the device stays out and shows the channel as unreachable.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum FloorError {
  #[error("the group carries another founding than the one pinned")]
  OtherFounding,
  #[error("properties version {offered} is below {floor} seen before")]
  OlderProperties { offered: u64, floor: u64 },
  #[error("properties version {0} with another hash than seen before")]
  OtherProperties(u64),
  #[error("epoch {offered} is below {floor} seen before")]
  OlderEpoch { offered: u64, floor: u64 },
  /// The conversation is pinned to an anchor, and the group state carries none.
  #[error("the group carries no anchor, but one is pinned")]
  Stripped,
}

/// The labelled hash of a channel name: over the UTF-8 bytes exactly as sent, with their length.
pub fn name_hash(name: &str) -> AnchorHash {
  // Invariant: a `VLByteSlice` serializes any length below 2^30 (a name is far shorter); an
  // over-long one still hashes deterministically through the length-prefixed fallback.
  let encoded = VLByteSlice(name.as_bytes())
    .tls_serialize_detached()
    .unwrap_or_else(|_| fallback_length_prefixed(name.as_bytes()));
  labeled_hash(domain::CHANNEL_NAME, &encoded)
}

/// The user's role for a properties change: a workspace owner or admin (of a roster judged as
/// `judged`) first, else a member with a leaf; `None` for anybody else, a meeting guest included
/// (a guest manages nothing in a group, §4.6, "Meeting guests"). Strict channels' admins (§4.6.2)
/// join once #1197's extension exists.
fn signer_class(
  signer: UserId,
  roster_version: u64,
  judged: Judged,
  tree_users: &BTreeSet<UserId>,
  directory: &impl IdentityDirectory,
  authority: &impl MembershipAuthority,
) -> Option<NameAuthority> {
  if directory.is_guest(signer) {
    None
  } else if authority.workspace_signer(signer, roster_version, judged) == Standing::Confirmed {
    Some(NameAuthority::Workspace)
  } else if tree_users.contains(&signer) {
    Some(NameAuthority::Member)
  } else {
    None
  }
}

/// Whether the device that signed a properties change is on its user's newest signed device list.
/// A change names no list version, so it is judged on the newest one: a revoked device's change
/// counts for nothing. `list_version` 0 with [`Judged::Newest`] means "on the newest list, of any
/// version" ([`IdentityDirectory::device_listing`]).
fn is_listed_signer(device: &VerifiedDevice, directory: &impl IdentityDirectory) -> bool {
  directory.device_listing(device, 0, Judged::Newest) == Standing::Confirmed
}

/// Whether a value the decoder didn't know sits in the founding's kind or origin, the parts a newer
/// app may add to (an update helps); an unknown value anywhere else is a malformed anchor.
fn has_newer_kind_or_origin(bytes: &[u8]) -> bool {
  // format (1) ‖ conversation_id (8) ‖ kind tag; a meeting chat's kind carries its meeting ID (8).
  const KIND: usize = 1 + 8;
  let origin = match bytes.get(KIND) {
    Some(1) => KIND + 1,
    Some(2) => KIND + 1 + 8,
    _ => return true,
  };
  !matches!(bytes.get(origin), Some(1..=3))
}

fn founding_payload(
  founding: &ChannelAnchorFounding,
  properties: &ChannelProperties,
) -> Result<Vec<u8>, AnchorError> {
  let mut payload = founding.tls_serialize_detached().map_err(AnchorError::Encoding)?;
  payload.extend(
    properties
      .tls_serialize_detached()
      .map_err(AnchorError::Encoding)?,
  );
  Ok(payload)
}

fn labeled_hash(label: &str, body: &[u8]) -> AnchorHash {
  AnchorHash(Sha256::digest(domain::labeled(label, body)).into())
}

fn fallback_length_prefixed(bytes: &[u8]) -> Vec<u8> {
  let mut out = (bytes.len() as u64).to_be_bytes().to_vec();
  out.extend_from_slice(bytes);
  out
}

/// Only the identity key the founding names may sign it.
struct OneKey {
  user_id: UserId,
  key: IdentityPublicKey,
}

impl IdentityDirectory for OneKey {
  fn pinned_identity(&self, user_id: UserId) -> Option<IdentityPublicKey> {
    (user_id == self.user_id).then_some(self.key)
  }
}

#[cfg(test)]
mod tests;
