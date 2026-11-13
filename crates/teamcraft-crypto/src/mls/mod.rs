//! MLS (RFC 9420, `openmls`) for key distribution (e2ee.md §4.1, §4.6).
//!
//! Every conversation is one MLS group and every device a member. MLS carries only key material:
//! content key bundles travel as MLS application messages, message bodies are encrypted with the
//! content keys ([`crate::message`]). Membership changes are only committed – and only accepted
//! from other committers – when they carry a valid [`SignedMembershipChange`] in the commit's
//! authenticated data, or are additions of further cross-signed devices of an existing member.
//!
//! [`SignedMembershipChange`]: crate::membership::SignedMembershipChange

mod content_keys;
mod credential;
mod group;
mod rejoin;
mod stored_key_packages;

use std::fmt;

use openmls::prelude::{
  Capabilities, Ciphersuite, ExtensionType, KeyPackage, KeyPackageIn, Lifetime, MlsMessageIn,
  ProtocolMessage, ProtocolVersion,
};
use openmls_traits::OpenMlsProvider;
use tls_codec::{Deserialize as _, Serialize as _};

pub use content_keys::{ContentKeyBundle, SealedContentKeys};
pub use group::{
  ChangeHistory, CommitOutput, ConversationGroup, GroupEvent, MAX_CHANGE_AGE_EPOCHS, MembershipUpdate,
  PendingCommit, StagedJoin, StoredGroupState, VerifiedWelcome,
};
pub use rejoin::{RejoinRequest, SignedRejoinRequest, VerifiedRejoinRequest};
pub use stored_key_packages::{StoredKeyPackage, delete_key_package, key_package_ref, stored_key_packages};

use crate::channel_anchor::{AnchorError, CHANNEL_ANCHOR_EXTENSION_TYPE, PropertiesChangeError};
use crate::identity::{DeviceKeyPair, IdentityError, SignedDeviceCertificate, VerifiedDevice};
use crate::ids::{ConversationId, DeviceId, UserId};
use crate::membership::{
  ChangeId, IdentityDirectory, MembershipAction, MembershipError, SignedMembershipChange,
};
use crate::strict_roles::{STRICT_ROLES_EXTENSION_TYPE, StrictRolesError};

/// The only ciphersuite: X25519, ChaCha20-Poly1305, SHA-256, Ed25519 (matches the device keys).
pub const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519;

/// How long a key package is valid after its creation. With [`KEY_PACKAGE_BACKDATE`] this is the
/// longest range `openmls` accepts (84 days and an hour). Clients replace packages long before
/// (e2ee.md §4.3).
pub const KEY_PACKAGE_LIFETIME_SECS: u64 = 83 * DAY_SECS;
/// How far a key package's validity starts before its creation, so a creator whose clock runs up to
/// a day fast still uploads packages others accept.
pub const KEY_PACKAGE_BACKDATE_SECS: u64 = DAY_SECS;
const DAY_SECS: u64 = 24 * 60 * 60;

/// The default `openmls` provider (RustCrypto primitives, in-memory storage). Clients will plug
/// in persistent storage behind the same `OpenMlsProvider` trait.
pub type DefaultMlsProvider = openmls_rust_crypto::OpenMlsRustCrypto;

/// A member device of a group, as identified by its verified credential.
#[derive(
  Clone,
  Copy,
  Debug,
  PartialEq,
  Eq,
  Hash,
  PartialOrd,
  Ord,
  tls_codec::TlsSerialize,
  tls_codec::TlsDeserializeBytes,
  tls_codec::TlsSize,
)]
pub struct GroupMember {
  pub user_id: UserId,
  pub device_id: DeviceId,
}

#[derive(Debug, thiserror::Error)]
pub enum MlsError {
  #[error("MLS {operation} failed: {message}")]
  Openmls {
    operation: &'static str,
    message: String,
  },
  #[error("group belongs to conversation {expected}, got {actual}")]
  WrongConversation {
    expected: ConversationId,
    actual: ConversationId,
  },
  #[error("credential is not a TeamCraft device credential")]
  UnsupportedCredential,
  #[error("no pinned identity key for user {0}")]
  UnknownUser(UserId),
  #[error("credential failed verification: {0}")]
  InvalidCredential(IdentityError),
  #[error("membership change rejected: {0}")]
  Membership(#[from] MembershipError),
  #[error("device {0} isn't in its user's signed device list")]
  InactiveDevice(DeviceId),
  #[error("device {0} was removed from this group and can't come back")]
  RemovedDevice(DeviceId),
  #[error("commit adds or removes members without a signed membership change")]
  MissingSignedChange,
  #[error("membership change {0:?} was already applied")]
  ReplayedChange(ChangeId),
  #[error("membership change signed in epoch {signed_epoch} is stale in epoch {current_epoch}")]
  StaleChange { signed_epoch: u64, current_epoch: u64 },
  #[error("commit does not match its signed membership change")]
  ChangeDoesNotMatchCommit,
  #[error("a guest's commit may only change the guest's own devices")]
  GuestOverreach,
  #[error("commit contains a proposal type TeamCraft does not use")]
  UnsupportedProposal,
  #[error("commit or message from a sender outside the group")]
  ExternalSender,
  #[error("a member's update changes its user or device identity")]
  IdentityChangedInUpdate,
  #[error("unexpected MLS message: {0}")]
  UnexpectedMessage(&'static str),
  #[error("there is no pending commit to confirm")]
  NoPendingCommit,
  /// Joining by external commit: the group already has a leaf of this device (a join whose answer
  /// was lost, or state this device lost). It has to be added back instead.
  #[error("this device is in the group already")]
  AlreadyInGroup,
  /// A welcome for a key package this device doesn't hold (any more): used up, deleted after it
  /// expired or was replaced (e2ee.md §4.3.1), or never this device's. It has to be added back.
  #[error("no key package of this device matches the welcome")]
  NoMatchingKeyPackage,
  /// Joining by external commit: the group information is older than an epoch this user's devices
  /// already saw (a server handing out an old one).
  #[error("group information for epoch {epoch} is older than epoch {floor} seen before")]
  StaleGroupInfo { epoch: u64, floor: u64 },
  #[error("HPKE failed: {0}")]
  Hpke(String),
  /// Content keys sealed to a device didn't open there: for another device, tampered with, or
  /// moved to another group, epoch or sender.
  #[error("sealed content keys rejected")]
  SealedKeysRejected,
  /// The group's anchor doesn't verify; the group is treated as not anchored.
  #[error("channel anchor: {0}")]
  Anchor(#[from] AnchorError),
  #[error("properties change refused: {0}")]
  PropertiesChange(#[from] PropertiesChangeError),
  /// A commit changes the group context beyond what its signed properties change says.
  #[error("commit changes the group context beyond the anchor's properties")]
  ContextChangeMismatch,
  /// The device's app doesn't announce what the group requires (the channel anchor): its user has
  /// to update before they can be added.
  #[error("device {0} has to update its app to join this group")]
  MustUpdate(DeviceId),
  /// A properties change for a group without an anchor (adding one is #1728's review).
  #[error("the group has no anchor to change")]
  NotAnchored,
  /// A strict channel's roles, or a change of them, don't hold (e2ee.md §4.6.2).
  #[error("strict roles: {0}")]
  StrictRoles(#[from] StrictRolesError),
  /// A strict channel's rule refuses the commit's membership change: its signer may not make it
  /// (e2ee.md §4.6.2, "Who may do what"). The device stops at this epoch (D4); it doesn't ask to
  /// be added back.
  #[error("{signer} may not {action:?} in this strict channel")]
  StrictRefused {
    signer: UserId,
    action: MembershipAction,
  },
  /// Judging the commit's signer needs a roster this device can't verify yet: hold the commit
  /// back and read the roster again. Not a refusal (e2ee.md §4.6.2, "Workspace signers").
  #[error("{0} signed as a workspace admin; the roster has to be read first")]
  StrictRosterNeeded(UserId),
  /// A welcome back into a strict channel from somebody who is neither an admin of the roles this
  /// device knew nor a workspace owner or admin: it isn't opened (e2ee.md §4.6.2, "Falling out").
  #[error("a welcome into this strict channel from {0}, who isn't an admin of it")]
  StrictWelcomeRefused(UserId),
  #[error("encoding failed: {0}")]
  Encoding(#[from] tls_codec::Error),
}

/// What the server and the owning device need to know about an uploaded key package, read from the
/// package itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyPackageInfo {
  /// Seconds since the Unix epoch after which nobody accepts the package any more.
  pub not_after_unix_secs: u64,
  pub is_last_resort: bool,
}

/// Creates a key package for this device and stores its private part in the provider's storage.
/// Upload the returned bytes to the server so others can add this device. `now_unix_secs` is the
/// creator's clock (injectable for tests); the package is valid for [`KEY_PACKAGE_LIFETIME_SECS`].
pub fn create_key_package(
  provider: &impl OpenMlsProvider,
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
  now_unix_secs: u64,
) -> Result<Vec<u8>, MlsError> {
  let builder = KeyPackage::builder()
    .leaf_node_capabilities(leaf_capabilities(&[]))
    .key_package_lifetime(lifetime_from(now_unix_secs));
  build_key_package(provider, device, certificate, builder)
}

/// Creates the device's last-resort key package (RFC 9420 §16.8): the server hands it out when the
/// regular ones are used up, and its private key survives joining, so it can be used repeatedly.
/// The device replaces it regularly (e2ee.md §4.3).
pub fn create_last_resort_key_package(
  provider: &impl OpenMlsProvider,
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
  now_unix_secs: u64,
) -> Result<Vec<u8>, MlsError> {
  // The leaf must announce support for the last_resort extension the key package carries.
  let builder = KeyPackage::builder()
    .leaf_node_capabilities(leaf_capabilities(&[ExtensionType::LastResort]))
    .key_package_lifetime(lifetime_from(now_unix_secs))
    .mark_as_last_resort();
  build_key_package(provider, device, certificate, builder)
}

/// Checks an uploaded key package (encoding, signature, ciphersuite, lifetime against this
/// machine's clock) and reads its expiry. The server records the expiry so it never hands out an
/// expired package; it does not check the credential, which members do when they add the device.
pub fn inspect_key_package(bytes: &[u8]) -> Result<KeyPackageInfo, MlsError> {
  let crypto = openmls_rust_crypto::RustCrypto::default();
  let key_package = KeyPackageIn::tls_deserialize_exact(bytes)?
    .validate(&crypto, ProtocolVersion::Mls10)
    .map_err(|error| MlsError::openmls("key package validation", error))?;
  if key_package.ciphersuite() != CIPHERSUITE {
    return Err(MlsError::UnexpectedMessage(
      "key package with another ciphersuite",
    ));
  }
  Ok(KeyPackageInfo {
    not_after_unix_secs: key_package.life_time().not_after(),
    is_last_resort: key_package.last_resort(),
  })
}

/// The users the signed membership change of a group message names: the user it adds or removes,
/// and its signer. Read from the message's authenticated data, which MLS sends unencrypted, and
/// verified by nothing here: a member pins their identity keys before it processes the message,
/// which verifies the change as always. A commit read hours later can add somebody who has left
/// again since, whom the conversation's current member list no longer names (issue #972). Empty
/// for a message without a signed change.
pub fn users_named_in_change(message: &[u8]) -> Vec<UserId> {
  let Ok(protocol) =
    MlsMessageIn::tls_deserialize_exact(message).map(MlsMessageIn::try_into_protocol_message)
  else {
    return Vec::new();
  };
  let Ok(ProtocolMessage::PrivateMessage(private)) = protocol else {
    return Vec::new();
  };
  let Ok(signed) = SignedMembershipChange::from_bytes(private.aad()) else {
    return Vec::new();
  };
  let target = match signed.unverified().action() {
    MembershipAction::AddUser { user_id, .. }
    | MembershipAction::RemoveUser { user_id }
    | MembershipAction::RemoveDevice { user_id, .. } => *user_id,
  };
  vec![target, signed.signer_user()]
}

/// The capabilities every leaf of this device announces: the channel anchor (e2ee.md §4.6.5,
/// "Who can be in an anchored group"), so the device can be added to anchored groups, plus
/// `extra`. Without it openmls refuses the leaf wherever a group requires the anchor.
pub(crate) fn leaf_capabilities(extra: &[ExtensionType]) -> Capabilities {
  let mut extensions = extra.to_vec();
  extensions.push(ExtensionType::Unknown(CHANNEL_ANCHOR_EXTENSION_TYPE));
  if ANNOUNCES_STRICT_ROLES {
    extensions.push(ExtensionType::Unknown(STRICT_ROLES_EXTENSION_TYPE));
  }
  Capabilities::new(None, None, Some(&extensions), None, None)
}

/// Whether this build's leaves announce strict channels (e2ee.md §4.6.2, "Older clients"): a
/// client announces the capability only if it implements the whole rule, and the parts outside
/// this crate (stopping at a refused commit, the welcome's sender check, the rewind) are the app's.
/// So it is the `strict-channels` feature, which the app turns on once it implements them; this
/// crate's own tests always announce it.
pub const ANNOUNCES_STRICT_ROLES: bool = cfg!(any(test, feature = "strict-channels"));

fn lifetime_from(now_unix_secs: u64) -> Lifetime {
  Lifetime::init(
    now_unix_secs.saturating_sub(KEY_PACKAGE_BACKDATE_SECS),
    now_unix_secs.saturating_add(KEY_PACKAGE_LIFETIME_SECS),
  )
}

fn build_key_package(
  provider: &impl OpenMlsProvider,
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
  builder: openmls::prelude::KeyPackageBuilder,
) -> Result<Vec<u8>, MlsError> {
  let credential = credential::credential_with_key(certificate)?;
  let bundle = builder
    .build(CIPHERSUITE, provider, device, credential)
    .map_err(|error| MlsError::openmls("key package creation", error))?;
  Ok(bundle.key_package().tls_serialize_detached()?)
}

/// Decodes, validates (signature, lifetime, ciphersuite) and checks the credential of a key
/// package fetched from the server.
pub(crate) fn validate_key_package(
  provider: &impl OpenMlsProvider,
  bytes: &[u8],
  directory: &impl IdentityDirectory,
) -> Result<(KeyPackage, VerifiedDevice), MlsError> {
  let key_package_in = KeyPackageIn::tls_deserialize_exact(bytes)?;
  let key_package = key_package_in
    .validate(provider.crypto(), ProtocolVersion::Mls10)
    .map_err(|error| MlsError::openmls("key package validation", error))?;
  if key_package.ciphersuite() != CIPHERSUITE {
    return Err(MlsError::UnexpectedMessage(
      "key package with another ciphersuite",
    ));
  }
  let leaf = key_package.leaf_node();
  let device = credential::verify(leaf.credential(), leaf.signature_key().as_slice(), directory)?;
  if !directory.is_active_device(&device) {
    return Err(MlsError::InactiveDevice(device.device_id()));
  }
  Ok((key_package, device))
}

impl MlsError {
  /// Whether a strict channel's commit was held back for a roster this device has to read first,
  /// rather than refused: retry it after the read; don't stop the group at it (e2ee.md §4.6.2).
  pub fn is_strict_retry(&self) -> bool {
    matches!(
      self,
      Self::StrictRosterNeeded(_) | Self::StrictRoles(StrictRolesError::RosterUnknown(_))
    )
  }

  fn openmls(operation: &'static str, error: impl fmt::Display) -> Self {
    Self::Openmls {
      operation,
      message: error.to_string(),
    }
  }
}

impl From<&VerifiedDevice> for GroupMember {
  fn from(device: &VerifiedDevice) -> Self {
    Self {
      user_id: device.user_id(),
      device_id: device.device_id(),
    }
  }
}
