//! Strict channels (e2ee.md §4.6.2, issue #1197): a channel whose membership changes only signed
//! channel admins (and moderators, for plain members) may make, with the switch and the roles in
//! the MLS group context.
//!
//! The roles live in the group context extension [`STRICT_ROLES_EXTENSION_TYPE`]: every member of
//! an epoch holds the same context (it goes into the key schedule), so every member judges a commit
//! from the same data and the server can't show two versions of the rule. The extension holds the
//! last [`SignedStrictRolesChange`], which verifies on its own: the roles, their version and the
//! chain hash, signed by a channel admin of the context or by a workspace owner or admin.
//!
//! This module is the pure part: the encoding, the hash chain, signing, and the two verdicts,
//! [`StrictRoles::check_change`] (who may change the roles) and [`membership_verdict`] (who may
//! add or remove whom). Where the extension sits in the group, and how a commit's signer is tied
//! to a leaf, is [`crate::mls::ConversationGroup`]'s.

use sha2::{Digest, Sha256};
use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize, VLBytes};

use crate::channel_anchor::{Judged, Standing};
use crate::domain;
use crate::identity::{DeviceKeyPair, IdentityPublicKey, SignedDeviceCertificate, VerifiedDevice};
use crate::ids::{ConversationId, UserId};
use crate::membership::{IdentityDirectory, MembershipAction, MembershipAuthority};
use crate::workspace::{WorkspaceLog, WorkspaceLogError, claimed_signer, sign_log_entry, verify_log_entry};

/// The private-use group context extension type of a strict channel's roles (RFC 9420 §17.3).
/// Present means strict; absent means §4.6. Nothing else switches it.
pub const STRICT_ROLES_EXTENSION_TYPE: u16 = 0xF7C0;
/// The encoding this code writes and reads. A newer one: the app has to update (and a leaf of
/// this version never announced it, so it can't be in such a group).
pub const STRICT_ROLES_FORMAT: u8 = 1;
/// Default of the client setting `max_strict_roles`: admins and moderators one context names. The
/// extension travels in every welcome and group information, so it stays small. A constant for the
/// verdict: every member must decide alike, or the group splits.
pub const MAX_STRICT_ROLES: usize = 64;
/// A larger extension isn't parsed (64 roles of about 50 bytes, the signed entry and headroom).
pub const MAX_STRICT_ROLES_BYTES: usize = 8 * 1024;
/// How many epochs after `signed_for_epoch` a roles change may still be committed (§4.6,
/// "Freshness"). A constant, not a setting, for the same reason.
pub const STRICT_CHANGE_WINDOW_EPOCHS: u64 = 32;
/// Marks a commit's authenticated data as a [`SignedStrictRolesChange`].
pub const STRICT_ROLES_AAD_MAGIC: &[u8] = b"TCSR";

/// A role a strict channel's context names. Members are implicit and never listed.
#[derive(
  Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, TlsSerialize, TlsDeserializeBytes, TlsSize,
)]
#[repr(u8)]
pub enum StrictRoleKind {
  Admin = 1,
  /// May remove plain members and add nobody (D1).
  Moderator = 2,
}

/// One person's role, with the identity key their signatures are verified against: the context's,
/// not the server's directory, so every member verifies alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct StrictRole {
  pub user_id: UserId,
  pub role: StrictRoleKind,
  pub identity_key: IdentityPublicKey,
}

/// In which role a roles change was signed. Signed, so a change can't be passed off under the
/// other rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
#[repr(u8)]
pub enum StrictSignerRole {
  /// An admin of the context being changed, with a leaf in the tree.
  ChannelAdmin = 1,
  /// A workspace owner or admin of a fresh roster at `roster_version`.
  Workspace = 2,
}

/// A change of a strict channel's roles, the payload of a [`WorkspaceLog::StrictRoles`] entry
/// (e2ee.md §4.6.2, "Role changes while strict"). With `roles` empty it turns strict off.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct StrictRolesChange {
  pub conversation_id: ConversationId,
  /// One up per change. The change that turns strict on starts the chain at any version.
  pub version: u64,
  /// [`StrictRolesChange::hash`] of the change this one replaces; zero for turning strict on.
  pub prev_hash: StrictHash,
  /// The epoch the signer saw; accepted up to [`STRICT_CHANGE_WINDOW_EPOCHS`] later.
  pub signed_for_epoch: u64,
  pub signer_role: StrictSignerRole,
  /// For a workspace signer: the roster that made them one; 0 otherwise.
  pub roster_version: u64,
  /// The channel role log entry (§4.6.1) this mirrors, for the UI and the history: never decided
  /// on.
  pub role_log_version: u64,
  pub role_log_hash: StrictHash,
  /// Ascending by user, each user once, at most [`MAX_STRICT_ROLES`].
  pub roles: Vec<StrictRole>,
}

/// A roles change with its signature: the extension's content, and the authenticated data of the
/// commit that makes it (after [`STRICT_ROLES_AAD_MAGIC`]).
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SignedStrictRolesChange {
  change: StrictRolesChange,
  /// A workspace log entry signature (kind [`WorkspaceLog::StrictRoles`], version = the change's)
  /// over `change`.
  entry: VLBytes,
}

/// The extension as it sits in the group context.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct StrictRoles {
  format: u8,
  last_change: SignedStrictRolesChange,
}

/// A SHA-256 value of the chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct StrictHash([u8; 32]);

/// The signer of a membership change as the verdict sees them (e2ee.md §4.6.2, "Who may do what").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrictSigner {
  /// An admin of the context, signing with a device that has a leaf.
  ChannelAdmin,
  /// A moderator of the context, signing with a device that has a leaf.
  Moderator,
  /// A workspace owner or admin of a fresh roster.
  Workspace,
  /// Anybody else: a member, guest, space admin or scheme holder (D2), a role holder without a
  /// leaf, or a user outside the group.
  Other,
}

/// Why a strict channel's roles or one of its commits are refused.
#[derive(Debug, thiserror::Error)]
pub enum StrictRolesError {
  #[error("strict roles of {size} bytes are larger than {max}")]
  TooLarge { size: usize, max: usize },
  /// A newer format or role: the app has to update.
  #[error("strict roles in a newer format; the app has to update")]
  MustUpdate,
  #[error("strict roles don't parse: {0}")]
  Malformed(tls_codec::Error),
  #[error("roles change for conversation {0}, not this one")]
  WrongConversation(ConversationId),
  #[error("{0} roles, more than {MAX_STRICT_ROLES}")]
  TooManyRoles(usize),
  #[error("roles aren't ascending by user, each once")]
  UnorderedRoles,
  #[error("strict roles without an admin")]
  NoAdmin,
  #[error("no admin of the new roles has a leaf in the group")]
  NoAdminWithLeaf,
  #[error("roles change to version {offered}, expected {expected}")]
  WrongVersion { expected: u64, offered: u64 },
  #[error("roles change doesn't continue the current roles")]
  BrokenChain,
  #[error("roles change signed in epoch {signed_epoch} is stale in epoch {current_epoch}")]
  Stale { signed_epoch: u64, current_epoch: u64 },
  #[error("roles change signature: {0}")]
  Signature(WorkspaceLogError),
  #[error("{0} may not make this roles change")]
  NotAllowed(UserId),
  /// The roster that makes the signer a workspace owner or admin can't be verified yet: hold the
  /// commit back and read the roster again; it isn't refused for that.
  #[error("roster version {0} can't be verified yet")]
  RosterUnknown(u64),
  #[error("strict is off; there is nothing to turn off")]
  NotStrict,
  /// Only channels: a meeting chat never becomes strict (and DMs and the notes are the caller's
  /// to keep out).
  #[error("only a channel can be strict")]
  NotAChannel,
  #[error("signing failed: {0}")]
  Signing(WorkspaceLogError),
  #[error("encoding failed: {0}")]
  Encoding(tls_codec::Error),
}

/// What a verified roles change does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StrictRolesVerdict {
  /// The channel turns strict, or its roles change.
  Roles(StrictRoles),
  /// Strict is turned off: the extension goes.
  Off,
}

impl StrictRolesChange {
  /// The labelled hash the next change's `prev_hash` holds: over the change without its
  /// signature, so a re-signed change of the same content chains the same.
  pub fn hash(&self) -> Result<StrictHash, StrictRolesError> {
    let encoded = self
      .tls_serialize_detached()
      .map_err(StrictRolesError::Encoding)?;
    Ok(StrictHash(
      Sha256::digest(domain::labeled(domain::STRICT_ROLES, &encoded)).into(),
    ))
  }

  /// Signs the change with the device's key; `certificate` is that device's certificate.
  pub fn sign(
    self,
    device: &DeviceKeyPair,
    certificate: &SignedDeviceCertificate,
  ) -> Result<SignedStrictRolesChange, StrictRolesError> {
    let payload = self
      .tls_serialize_detached()
      .map_err(StrictRolesError::Encoding)?;
    let entry = sign_log_entry(
      device,
      certificate,
      WorkspaceLog::StrictRoles,
      self.version,
      &payload,
    )
    .map_err(StrictRolesError::Signing)?;
    Ok(SignedStrictRolesChange {
      change: self,
      entry: VLBytes::new(entry),
    })
  }

  /// Roles ascending by user, each once, within [`MAX_STRICT_ROLES`].
  fn check_roles(&self) -> Result<(), StrictRolesError> {
    if self.roles.len() > MAX_STRICT_ROLES {
      return Err(StrictRolesError::TooManyRoles(self.roles.len()));
    }
    let is_ascending = self
      .roles
      .windows(2)
      .all(|pair| pair[0].user_id < pair[1].user_id);
    if !is_ascending {
      return Err(StrictRolesError::UnorderedRoles);
    }
    Ok(())
  }
}

impl SignedStrictRolesChange {
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, StrictRolesError> {
    if bytes.len() > MAX_STRICT_ROLES_BYTES {
      return Err(StrictRolesError::TooLarge {
        size: bytes.len(),
        max: MAX_STRICT_ROLES_BYTES,
      });
    }
    Self::tls_deserialize_exact_bytes(bytes).map_err(StrictRolesError::Malformed)
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, StrictRolesError> {
    self.tls_serialize_detached().map_err(StrictRolesError::Encoding)
  }

  /// The change, as signed; its signature is checked by [`StrictRoles::check_change`] only.
  pub fn change(&self) -> &StrictRolesChange {
    &self.change
  }

  /// The user the entry claims as signer, unverified.
  pub fn claimed_signer(&self) -> Result<UserId, WorkspaceLogError> {
    claimed_signer(self.entry.as_slice())
  }

  /// Verifies the entry under `keys`; returns the signing device.
  fn verify_signature(&self, keys: &impl IdentityDirectory) -> Result<VerifiedDevice, StrictRolesError> {
    let payload = self
      .change
      .tls_serialize_detached()
      .map_err(StrictRolesError::Encoding)?;
    verify_log_entry(
      self.entry.as_slice(),
      WorkspaceLog::StrictRoles,
      self.change.version,
      &payload,
      keys,
    )
    .map_err(StrictRolesError::Signature)
  }
}

impl StrictRoles {
  /// Parses the extension's content: within [`MAX_STRICT_ROLES_BYTES`], completely, in a known
  /// format, with valid roles. Its signature was checked when the commit that set it was judged
  /// (or, joining, is the caller's to judge with [`Self::check_change`] semantics).
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, StrictRolesError> {
    if bytes.len() > MAX_STRICT_ROLES_BYTES {
      return Err(StrictRolesError::TooLarge {
        size: bytes.len(),
        max: MAX_STRICT_ROLES_BYTES,
      });
    }
    match bytes.first() {
      None => return Err(StrictRolesError::Malformed(tls_codec::Error::EndOfStream)),
      Some(&format) if format != STRICT_ROLES_FORMAT => return Err(StrictRolesError::MustUpdate),
      Some(_) => {},
    }
    // An unknown value is a role or signer kind a newer app wrote.
    let roles = Self::tls_deserialize_exact_bytes(bytes).map_err(|error| match error {
      tls_codec::Error::UnknownValue(_) => StrictRolesError::MustUpdate,
      other => StrictRolesError::Malformed(other),
    })?;
    roles.last_change.change.check_roles()?;
    if roles.last_change.change.roles.is_empty() {
      // An empty list turns strict off: it is never in a context.
      return Err(StrictRolesError::NoAdmin);
    }
    Ok(roles)
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, StrictRolesError> {
    self.tls_serialize_detached().map_err(StrictRolesError::Encoding)
  }

  /// Roles from a change nobody checked, for tests that forge contexts.
  #[cfg(test)]
  pub(crate) fn for_tests(last_change: SignedStrictRolesChange) -> Self {
    Self {
      format: STRICT_ROLES_FORMAT,
      last_change,
    }
  }

  /// The change that set these roles.
  pub fn last_change(&self) -> &SignedStrictRolesChange {
    &self.last_change
  }

  pub fn version(&self) -> u64 {
    self.last_change.change.version
  }

  pub fn conversation_id(&self) -> ConversationId {
    self.last_change.change.conversation_id
  }

  /// The admins and moderators, ascending by user.
  pub fn roles(&self) -> &[StrictRole] {
    &self.last_change.change.roles
  }

  pub fn role_of(&self, user_id: UserId) -> Option<&StrictRole> {
    self.roles().iter().find(|role| role.user_id == user_id)
  }

  pub fn admins(&self) -> impl Iterator<Item = &StrictRole> {
    self
      .roles()
      .iter()
      .filter(|role| role.role == StrictRoleKind::Admin)
  }

  /// The hash a change of these roles must name as `prev_hash`.
  pub fn hash(&self) -> Result<StrictHash, StrictRolesError> {
    self.last_change.change.hash()
  }

  /// The verdict on a roles change committed in `epoch` to a group whose current roles are
  /// `maybe_current` (`None`: not strict yet) and whose tree holds `leaves` (e2ee.md §4.6.2,
  /// "Turning it on", "Role changes while strict"). Reads only the context, the tree and, for a
  /// workspace signer, the roster, so every member decides alike.
  ///
  /// - Turning on: a workspace signer (D3), a zero `prev_hash`, at least one admin.
  /// - A change while strict: version one up and `prev_hash` the current roles' hash, signed by an
  ///   admin of the current roles whose signing device has a leaf, or by a workspace signer.
  /// - Turning off: the same signers, with no roles.
  /// - Every change: fresh, for this conversation, its roles in order; new roles keep an admin
  ///   with a leaf, so a channel can't be handed to nobody.
  pub fn check_change(
    maybe_current: Option<&Self>,
    signed: &SignedStrictRolesChange,
    conversation_id: ConversationId,
    epoch: u64,
    leaves: &[TreeLeaf],
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<StrictRolesVerdict, StrictRolesError> {
    let change = &signed.change;
    if change.conversation_id != conversation_id {
      return Err(StrictRolesError::WrongConversation(change.conversation_id));
    }
    change.check_roles()?;
    if change.signed_for_epoch > epoch || epoch - change.signed_for_epoch > STRICT_CHANGE_WINDOW_EPOCHS {
      return Err(StrictRolesError::Stale {
        signed_epoch: change.signed_for_epoch,
        current_epoch: epoch,
      });
    }
    match maybe_current {
      None => {
        if change.roles.is_empty() {
          return Err(StrictRolesError::NotStrict);
        }
        if change.prev_hash != StrictHash::ZERO {
          return Err(StrictRolesError::BrokenChain);
        }
        if change.signer_role != StrictSignerRole::Workspace {
          return Err(StrictRolesError::NotAllowed(signed.claimed_signer_or_nobody()));
        }
      },
      Some(current) => {
        let expected = current.version().saturating_add(1);
        if change.version != expected {
          return Err(StrictRolesError::WrongVersion {
            expected,
            offered: change.version,
          });
        }
        if change.prev_hash != current.hash()? {
          return Err(StrictRolesError::BrokenChain);
        }
      },
    }
    match change.signer_role {
      StrictSignerRole::ChannelAdmin => {
        let current = maybe_current.ok_or(StrictRolesError::NotStrict)?;
        let claimed = signed.claimed_signer_or_nobody();
        let role = current
          .role_of(claimed)
          .filter(|role| role.role == StrictRoleKind::Admin)
          .ok_or(StrictRolesError::NotAllowed(claimed))?;
        if change.roster_version != 0 {
          return Err(StrictRolesError::NotAllowed(claimed));
        }
        let device = signed.verify_signature(&OneKey {
          user_id: role.user_id,
          key: role.identity_key,
        })?;
        if !has_leaf(&device, leaves) {
          return Err(StrictRolesError::NotAllowed(claimed));
        }
      },
      StrictSignerRole::Workspace => {
        let device = signed.verify_signature(directory)?;
        let user = device.user_id();
        match authority.workspace_signer(user, change.roster_version, Judged::Newest) {
          Standing::Confirmed => {},
          Standing::Unknown => return Err(StrictRolesError::RosterUnknown(change.roster_version)),
          Standing::Refused => return Err(StrictRolesError::NotAllowed(user)),
        }
      },
    }
    if change.roles.is_empty() {
      return Ok(StrictRolesVerdict::Off);
    }
    let next = Self {
      format: STRICT_ROLES_FORMAT,
      last_change: signed.clone(),
    };
    if next.admins().next().is_none() {
      return Err(StrictRolesError::NoAdmin);
    }
    let has_admin_with_leaf = next
      .admins()
      .any(|admin| leaves.iter().any(|leaf| leaf.verifies_as(admin)));
    if !has_admin_with_leaf {
      return Err(StrictRolesError::NoAdminWithLeaf);
    }
    Ok(StrictRolesVerdict::Roles(next))
  }

  /// The signer class of a verified signing device, by these roles: an admin or moderator only
  /// if the device's certificate verified under the identity key the roles name and the device
  /// has a leaf (`leaves`). Workspace signers are the caller's to find (they need the roster).
  pub fn signer_class(&self, device: &VerifiedDevice, leaves: &[TreeLeaf]) -> StrictSigner {
    let Some(role) = self.role_of(device.user_id()) else {
      return StrictSigner::Other;
    };
    if device.identity_key() != &role.identity_key || !has_leaf(device, leaves) {
      return StrictSigner::Other;
    }
    match role.role {
      StrictRoleKind::Admin => StrictSigner::ChannelAdmin,
      StrictRoleKind::Moderator => StrictSigner::Moderator,
    }
  }
}

impl SignedStrictRolesChange {
  fn claimed_signer_or_nobody(&self) -> UserId {
    self.claimed_signer().unwrap_or(UserId::new(0))
  }
}

impl StrictHash {
  pub const ZERO: Self = Self([0; 32]);

  pub const fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
  }

  pub const fn as_bytes(&self) -> &[u8; 32] {
    &self.0
  }
}

/// A leaf of the group's tree as it stands, unverified: its certificate and the signature key it
/// signs MLS with. The strict rules verify a leaf against the identity key the context names,
/// never against the directory, so every member decides alike.
#[derive(Clone, Debug)]
pub struct TreeLeaf {
  pub certificate: SignedDeviceCertificate,
  pub signature_key: Vec<u8>,
}

impl TreeLeaf {
  /// Whether this leaf is a device of the role's user, certified by the role's identity key.
  fn verifies_as(&self, role: &StrictRole) -> bool {
    self.certificate.unverified().user_id == role.user_id
      && self
        .certificate
        .verify(&role.identity_key)
        .is_ok_and(|device| device.ensure_signing_key(&self.signature_key).is_ok())
  }
}

/// Who may add or remove whom in a strict channel (e2ee.md §4.6.2, "Who may do what"): a pure
/// function of the signer's class, the action and the target's role in the context.
///
/// | Signer           | Add | Remove a member | Remove a moderator or admin | Remove another's device |
/// |------------------|-----|-----------------|-----------------------------|-------------------------|
/// | Channel admin    | yes | yes             | yes                         | yes                     |
/// | Moderator (D1)   | no  | yes             | no                          | no                      |
/// | Workspace signer | yes | yes             | yes                         | yes                     |
/// | Anybody else     | no  | no              | no                          | no                      |
///
/// Leaving is always allowed: a user's own removal and the removal of their own device, whoever
/// they are. A workspace signer is further bound by the policy (`admin_self_join`), which the
/// caller checks.
pub fn membership_verdict(
  signer_user: UserId,
  signer: StrictSigner,
  action: &MembershipAction,
  maybe_target_role: Option<StrictRoleKind>,
) -> bool {
  let is_own = match *action {
    MembershipAction::RemoveUser { user_id } | MembershipAction::RemoveDevice { user_id, .. } => {
      user_id == signer_user
    },
    MembershipAction::AddUser { .. } => false,
  };
  if is_own {
    return true;
  }
  match signer {
    StrictSigner::ChannelAdmin | StrictSigner::Workspace => true,
    StrictSigner::Moderator => {
      matches!(action, MembershipAction::RemoveUser { .. }) && maybe_target_role.is_none()
    },
    StrictSigner::Other => false,
  }
}

/// Whether the (verified) device has a leaf in the tree: one of its user and device that signs
/// with the key the device's certificate names.
pub fn has_leaf(device: &VerifiedDevice, leaves: &[TreeLeaf]) -> bool {
  leaves.iter().any(|leaf| {
    let claimed = leaf.certificate.unverified();
    claimed.user_id == device.user_id()
      && claimed.device_id == device.device_id()
      && device.ensure_signing_key(&leaf.signature_key).is_ok()
  })
}

/// Only the identity key the roles name may sign as that role.
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
