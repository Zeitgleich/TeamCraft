//! Signed per-channel roles (issue #380, e2ee.md §4.6.1, admin.md §2.3).
//!
//! The server stores each channel's role log but makes no trust decision. Every entry is a full
//! snapshot of the channel's admins and moderators, signed by a device whose user may assign them
//! under the rules of [`teamcraft_proto::role_rules`], checked against signed data only:
//!
//! - **Workspace admins and owners** of the verified roster, fresh at the entry's server time, may
//!   sign any snapshot. Their entry may name another head than this device's (a checkpoint that
//!   re-anchors a device that skipped an entry).
//! - **Channel admins** of the snapshot this device accepted last, verified against the identity
//!   key that snapshot names for them; their entry must build on exactly that snapshot.
//! - **The founder**: while nothing was accepted yet, an entry whose signer names themselves as
//!   admin, with the identity key this device pinned for them (trust on first use, like the
//!   workspace root of trust). Channels created from this version on are founded by their creator;
//!   older channels by the first admin who changes a role.
//!
//! An entry that fails any check is skipped, not fatal: honest signers build on the last entry they
//! accepted, so a junk entry can't block the log. Versions only move forward, so the server can't
//! roll a device back or replay an entry; heads travel inside encrypted messages so members notice
//! a server showing them different logs ([`ChannelRoleState::note_head`]).
//!
//! The verified role is capped by the server's: the server may take a role away (it can withhold
//! anything anyway), never grant one ([`effective_role`]).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::ids as crypto;
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_crypto::workspace::{WorkspaceLog, log_entry_hash, verify_log_entry};
use teamcraft_proto::role_rules::{
  RoleActor, RoleChangeError, RoleTarget, SpaceStanding, check_channel_role_change,
};
use teamcraft_proto::{
  Base64Bytes, ChannelRole, ChannelRoleAssignment, ChannelRoleLogEntryResponse, ChannelRolesDocument,
  ConversationId, ConversationVisibility, MAX_CHANNEL_ROLE_ASSIGNMENTS, PermissionScheme, Role, UserId,
};

use crate::convert;
use crate::workspace::{RosterRole, WorkspaceState};

/// Length of an entry hash and of the zero hash a first entry names.
const HASH_BYTES: usize = 32;

/// What this device verified of one channel's role log.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelRoleState {
  /// Version of the last accepted entry; 0 while nothing is signed (a channel from before signed
  /// roles, whose roles are the server's word only).
  #[serde(default)]
  pub version: u64,
  #[serde(rename = "head_hash", default, skip_serializing_if = "Option::is_none")]
  pub maybe_head_hash: Option<Base64Bytes>,
  /// The accepted snapshot: everybody above member.
  #[serde(default)]
  pub assignments: Vec<ChannelRoleAssignment>,
  /// The newest version looked at, accepted or skipped; nothing at or below it is read again.
  #[serde(default)]
  pub cursor: u64,
  /// Why the last skipped entry was skipped.
  #[serde(rename = "rejection", default, skip_serializing_if = "Option::is_none")]
  pub maybe_rejection: Option<String>,
  /// A member verified another entry for the same version, or entries this device isn't shown.
  #[serde(rename = "alarm", default, skip_serializing_if = "Option::is_none")]
  pub maybe_alarm: Option<String>,
  /// A member's head newer than ours, to check after the next sync.
  #[serde(rename = "ahead", default, skip_serializing_if = "Option::is_none")]
  pub maybe_ahead: Option<ChannelRolesHead>,
}

/// The accepted head of a channel's role log; sent along inside the channel's encrypted messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelRolesHead {
  pub version: u64,
  pub hash: Base64Bytes,
}

/// On what authority an entry was (or would be) signed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignerAuthority {
  /// An owner or admin of the fresh, verified workspace roster.
  Workspace { is_owner: bool },
  /// An admin of the last accepted snapshot.
  ChannelAdmin,
  /// The first entry of a channel: its signer names themselves admin.
  Founder,
}

/// What verifying a channel's entries needs besides the log itself.
pub struct RoleVerifier<'a, D: IdentityDirectory> {
  pub conversation: ConversationId,
  pub workspace: &'a WorkspaceState,
  /// How old the newest signed roster may be for its admins to count.
  pub roster_max_age_ms: i64,
  /// Identity keys this device pinned: a founder must hold the key pinned for them.
  pub pinned: &'a D,
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelRoleError {
  #[error("role entry {version} has an unreadable payload: {reason}")]
  Payload { version: u64, reason: String },
  #[error("role entry {version} is for another channel")]
  OtherChannel { version: u64 },
  #[error("role entry {version} has an invalid snapshot: {reason}")]
  Snapshot { version: u64, reason: &'static str },
  #[error("role entry {version} isn't signed by somebody allowed to assign roles here")]
  Unauthorized { version: u64 },
  #[error("role entry {version} doesn't build on the roles this device verified")]
  BrokenChain { version: u64 },
  #[error("role entry {version} breaks a role rule for {user}: {rule}")]
  RoleRule {
    version: u64,
    user: UserId,
    rule: RoleChangeError,
  },
}

impl ChannelRoleState {
  /// The verified role of `user` (member when the snapshot doesn't name them).
  pub fn role_of(&self, user: UserId) -> ChannelRole {
    self
      .assignments
      .iter()
      .find(|assignment| assignment.user_id == user)
      .map_or(ChannelRole::Member, |assignment| assignment.role.clone())
  }

  /// Whether anything was signed for the channel; before that its roles are the server's word.
  pub fn is_signed(&self) -> bool {
    self.version > 0
  }

  pub fn head(&self) -> Option<ChannelRolesHead> {
    let hash = self.maybe_head_hash.clone()?;
    Some(ChannelRolesHead {
      version: self.version,
      hash,
    })
  }

  /// Verifies and applies new entries in version order. Each one is accepted or skipped; the
  /// reasons of the skipped ones are returned (and the last one kept).
  pub fn apply_entries<D: IdentityDirectory>(
    &mut self,
    mut entries: Vec<ChannelRoleLogEntryResponse>,
    verifier: &RoleVerifier<'_, D>,
  ) -> Vec<ChannelRoleError> {
    entries.sort_by_key(|entry| entry.version);
    let mut skipped = Vec::new();
    for entry in entries {
      if entry.version <= self.cursor {
        continue;
      }
      self.cursor = entry.version;
      if let Err(error) = self.apply_entry(&entry, verifier) {
        self.maybe_rejection = Some(error.to_string());
        skipped.push(error);
      }
    }
    skipped
  }

  /// Compares a member's head with ours: the same version with another hash means the server
  /// showed us different entries; a newer one is checked after the next sync.
  pub fn note_head(&mut self, other: &ChannelRolesHead) {
    if other.version == self.version && self.maybe_head_hash.as_ref() != Some(&other.hash) {
      self.maybe_alarm = Some(format!(
        "another member verified a different entry {} of this channel's roles: the server shows members different logs",
        other.version
      ));
    } else if other.version > self.version {
      self.maybe_ahead = Some(other.clone());
    }
  }

  /// After a sync: a member's newer head that this device still doesn't have means entries are
  /// held back from it, or it skipped an entry the member accepted.
  pub fn check_ahead(&mut self) {
    let Some(ahead) = self.maybe_ahead.take() else {
      return;
    };
    if ahead.version > self.cursor {
      self.maybe_alarm = Some(format!(
        "another member has entry {} of this channel's roles, which this device isn't shown",
        ahead.version
      ));
    } else if ahead.version > self.version {
      self.maybe_alarm = Some(format!(
        "another member accepted entry {} of this channel's roles, which this device refused",
        ahead.version
      ));
    } else {
      self.note_head(&ahead);
    }
  }

  /// On what authorities `signer` could sign the next entry now (`now_ms`: the time the roster's
  /// freshness is judged at), in the order members try them. Empty: an entry of theirs would be
  /// refused by every member.
  pub fn signing_authorities(
    &self,
    signer: UserId,
    workspace: &WorkspaceState,
    now_ms: i64,
    roster_max_age_ms: i64,
  ) -> Vec<SignerAuthority> {
    let mut authorities: Vec<SignerAuthority> =
      workspace_authority(workspace, signer, now_ms, roster_max_age_ms)
        .into_iter()
        .collect();
    if self.role_of(signer) == ChannelRole::Admin {
      authorities.push(SignerAuthority::ChannelAdmin);
    }
    if !self.is_signed() {
      authorities.push(SignerAuthority::Founder);
    }
    authorities
  }

  /// The next document: this device's snapshot with `changes` applied (`Member` drops the
  /// person), built on its head. `keys`: identity keys for people the snapshot doesn't name yet.
  pub fn next_document(
    &self,
    conversation: ConversationId,
    changes: &[(UserId, ChannelRole)],
    keys: &BTreeMap<UserId, [u8; 32]>,
    signed_at_ms: i64,
  ) -> Result<ChannelRolesDocument, ChannelRoleError> {
    let version = self.cursor + 1;
    let mut assignments = self.assignments.clone();
    for (user, role) in changes {
      assignments.retain(|assignment| assignment.user_id != *user);
      if *role == ChannelRole::Member {
        continue;
      }
      let key = keys.get(user).ok_or(ChannelRoleError::Snapshot {
        version,
        reason: "no identity key for a person given a role",
      })?;
      assignments.push(ChannelRoleAssignment {
        user_id: *user,
        role: role.clone(),
        identity_key: Base64Bytes::new(key.to_vec()),
      });
    }
    assignments.sort_by_key(|assignment| assignment.user_id);
    Ok(ChannelRolesDocument {
      version,
      conversation_id: conversation,
      assignments,
      prev_hash: self.head_or_zero(),
      signed_at_ms,
    })
  }

  /// The checks every member makes on a document built on this device's head, before the
  /// signature: what [`Self::apply_entries`] would refuse is never signed.
  pub fn check_document(
    &self,
    document: &ChannelRolesDocument,
    conversation: ConversationId,
    signer: UserId,
    authority: SignerAuthority,
  ) -> Result<(), ChannelRoleError> {
    let version = document.version;
    if document.conversation_id != conversation {
      return Err(ChannelRoleError::OtherChannel { version });
    }
    check_snapshot(version, &document.assignments)?;
    let continues = document.prev_hash == self.head_or_zero();
    match authority {
      // A checkpoint by the workspace's admins replaces whatever this device had.
      SignerAuthority::Workspace { .. } if !continues => return Ok(()),
      SignerAuthority::Workspace { .. } => {},
      SignerAuthority::ChannelAdmin if !continues => return Err(ChannelRoleError::BrokenChain { version }),
      SignerAuthority::ChannelAdmin => {
        if self.role_of(signer) != ChannelRole::Admin {
          return Err(ChannelRoleError::Unauthorized { version });
        }
      },
      SignerAuthority::Founder => {
        let is_admin_in_it = document
          .assignments
          .iter()
          .any(|assignment| assignment.user_id == signer && assignment.role == ChannelRole::Admin);
        if self.is_signed() || !continues || !is_admin_in_it {
          return Err(ChannelRoleError::Unauthorized { version });
        }
      },
    }
    self.check_changes(document, signer, authority)
  }

  fn apply_entry<D: IdentityDirectory>(
    &mut self,
    entry: &ChannelRoleLogEntryResponse,
    verifier: &RoleVerifier<'_, D>,
  ) -> Result<(), ChannelRoleError> {
    let version = entry.version;
    let document: ChannelRolesDocument =
      serde_json::from_slice(entry.payload.as_slice()).map_err(|error| ChannelRoleError::Payload {
        version,
        reason: error.to_string(),
      })?;
    if document.version != version {
      return Err(ChannelRoleError::Payload {
        version,
        reason: "the document names another version than its entry".into(),
      });
    }
    // The first authority the signature holds under and the document passes with decides; a
    // workspace admin founding a channel signs as its founder, for example.
    let mut outcome = Err(ChannelRoleError::Unauthorized { version });
    for (signer, authority) in self.verify_signer(entry, &document, verifier) {
      outcome = self.check_document(&document, verifier.conversation, signer, authority);
      if outcome.is_ok() {
        break;
      }
    }
    outcome?;
    let hash = log_entry_hash(
      WorkspaceLog::ChannelRoles,
      version,
      entry.payload.as_slice(),
      entry.signature.as_slice(),
    )
    .map_err(|error| ChannelRoleError::Payload {
      version,
      reason: error.to_string(),
    })?;
    self.version = version;
    self.maybe_head_hash = Some(Base64Bytes::new(hash.to_vec()));
    self.assignments = document.assignments;
    Ok(())
  }

  /// Who signed the entry and on what authorities the signature holds: the workspace roster, this
  /// device's snapshot and (before the first accepted entry) the founder's claim, in that order.
  fn verify_signer<D: IdentityDirectory>(
    &self,
    entry: &ChannelRoleLogEntryResponse,
    document: &ChannelRolesDocument,
    verifier: &RoleVerifier<'_, D>,
  ) -> Vec<(UserId, SignerAuthority)> {
    let version = entry.version;
    let mut found = Vec::new();
    let verify_with = |directory: &KeyDirectory| {
      verify_log_entry(
        entry.signature.as_slice(),
        WorkspaceLog::ChannelRoles,
        version,
        entry.payload.as_slice(),
        directory,
      )
      .ok()
      .and_then(|device| convert::user_from_crypto(device.user_id()).ok())
    };
    let at_ms = entry.created_at.0;
    if verifier
      .workspace
      .is_roster_fresh(at_ms, verifier.roster_max_age_ms)
    {
      let admins = KeyDirectory::from_keys(roster_admin_keys(verifier.workspace));
      if let Some(signer) = verify_with(&admins) {
        let is_owner = verifier
          .workspace
          .roster
          .members
          .iter()
          .any(|member| member.user_id == signer && member.role == RosterRole::Owner);
        found.push((signer, SignerAuthority::Workspace { is_owner }));
      }
    }
    let channel_admins = KeyDirectory::from_assignments(
      self
        .assignments
        .iter()
        .filter(|assignment| assignment.role == ChannelRole::Admin),
    );
    if let Some(signer) = verify_with(&channel_admins) {
      found.push((signer, SignerAuthority::ChannelAdmin));
    }
    if !self.is_signed() {
      // Only keys this device pinned itself: the server can't make up a founder of its own.
      let claimed = document.assignments.iter().filter(|assignment| {
        assignment.role == ChannelRole::Admin
          && verifier
            .pinned
            .pinned_identity(convert::user(assignment.user_id))
            .is_some_and(|pinned| pinned.as_bytes().as_slice() == assignment.identity_key.as_slice())
      });
      if let Some(signer) = verify_with(&KeyDirectory::from_assignments(claimed)) {
        found.push((signer, SignerAuthority::Founder));
      }
    }
    found
  }

  /// Every change against this device's snapshot follows the role rules for the signer: no own
  /// role changed (the founder's own claim aside), no last admin dropped, only known roles. The
  /// scheme is the built-in one; a space's own scheme is the server's word.
  fn check_changes(
    &self,
    document: &ChannelRolesDocument,
    signer: UserId,
    authority: SignerAuthority,
  ) -> Result<(), ChannelRoleError> {
    let (instance_role, maybe_own_role) = match authority {
      SignerAuthority::Workspace { is_owner } => (
        if is_owner { Role::Owner } else { Role::Admin },
        Some(self.role_of(signer)),
      ),
      SignerAuthority::ChannelAdmin | SignerAuthority::Founder => (Role::Member, Some(ChannelRole::Admin)),
    };
    let actor = RoleActor {
      instance_role,
      is_space_admin: false,
      maybe_own_role,
      visibility: ConversationVisibility::Private,
      is_archived: false,
      maybe_granted: None,
    };
    let admin_count = self
      .assignments
      .iter()
      .filter(|assignment| assignment.role == ChannelRole::Admin)
      .count();
    let next = ChannelRoleState {
      assignments: document.assignments.clone(),
      ..Self::default()
    };
    let users: BTreeSet<UserId> = self
      .assignments
      .iter()
      .chain(&document.assignments)
      .map(|assignment| assignment.user_id)
      .collect();
    for user in users {
      let (current, new_role) = (self.role_of(user), next.role_of(user));
      if current == new_role || (authority == SignerAuthority::Founder && user == signer) {
        continue;
      }
      let target = RoleTarget {
        is_guest: false,
        space: SpaceStanding::Unknown,
        is_self: user == signer,
        current,
      };
      check_channel_role_change(
        &PermissionScheme::default(),
        &actor,
        &target,
        &new_role,
        admin_count,
      )
      .map_err(|rule| ChannelRoleError::RoleRule {
        version: document.version,
        user,
        rule,
      })?;
    }
    Ok(())
  }

  fn head_or_zero(&self) -> Base64Bytes {
    self
      .maybe_head_hash
      .clone()
      .unwrap_or_else(|| Base64Bytes::new(vec![0; HASH_BYTES]))
  }
}

/// The role to act on: the verified one, but never more than the server lists (the server can
/// take a role away, as it can withhold anything, but never grant one). A guest of the instance is
/// a member whatever is signed (role_rules: guests stay members).
pub fn effective_role(verified: &ChannelRole, server: &ChannelRole, is_guest: bool) -> ChannelRole {
  if is_guest {
    return ChannelRole::Member;
  }
  if rank(verified) <= rank(server) {
    verified.clone()
  } else {
    server.clone()
  }
}

fn rank(role: &ChannelRole) -> u8 {
  match role {
    ChannelRole::Admin => 2,
    ChannelRole::Moderator => 1,
    ChannelRole::Member | ChannelRole::Unknown(_) => 0,
  }
}

/// A snapshot names everybody once, with a known role above member and a usable key, and keeps
/// at least one admin (a channel never loses its last admin, role_rules).
fn check_snapshot(version: u64, assignments: &[ChannelRoleAssignment]) -> Result<(), ChannelRoleError> {
  let invalid = |reason| Err(ChannelRoleError::Snapshot { version, reason });
  if assignments.len() > MAX_CHANNEL_ROLE_ASSIGNMENTS {
    return invalid("too many assignments");
  }
  let mut users = BTreeSet::new();
  for assignment in assignments {
    if !users.insert(assignment.user_id) {
      return invalid("a person is named twice");
    }
    if !matches!(assignment.role, ChannelRole::Admin | ChannelRole::Moderator) {
      return invalid("only admins and moderators are listed");
    }
    if assignment.identity_key.len() != 32 {
      return invalid("an identity key has the wrong length");
    }
  }
  if !assignments
    .iter()
    .any(|assignment| assignment.role == ChannelRole::Admin)
  {
    return invalid("no admin");
  }
  Ok(())
}

/// Identity keys of the roster's owners and admins: they may sign any channel's roles.
fn roster_admin_keys(workspace: &WorkspaceState) -> BTreeMap<crypto::UserId, IdentityPublicKey> {
  workspace
    .roster
    .members
    .iter()
    .filter(|member| matches!(member.role, RosterRole::Owner | RosterRole::Admin))
    .filter_map(|member| {
      let key = <[u8; 32]>::try_from(member.identity_key.as_slice()).ok()?;
      Some((convert::user(member.user_id), IdentityPublicKey::from_bytes(key)))
    })
    .collect()
}

/// [`SignerAuthority::Workspace`] if `user` is an owner or admin of the roster, fresh at `at_ms`.
fn workspace_authority(
  workspace: &WorkspaceState,
  user: UserId,
  at_ms: i64,
  roster_max_age_ms: i64,
) -> Option<SignerAuthority> {
  if !workspace.is_roster_fresh(at_ms, roster_max_age_ms) {
    return None;
  }
  let member = workspace
    .roster
    .members
    .iter()
    .find(|member| member.user_id == user)?;
  match member.role {
    RosterRole::Owner => Some(SignerAuthority::Workspace { is_owner: true }),
    RosterRole::Admin => Some(SignerAuthority::Workspace { is_owner: false }),
    RosterRole::Unknown => None,
  }
}

/// Identity keys as an [`IdentityDirectory`], for one verification attempt.
struct KeyDirectory(BTreeMap<crypto::UserId, IdentityPublicKey>);

impl KeyDirectory {
  fn from_keys(keys: BTreeMap<crypto::UserId, IdentityPublicKey>) -> Self {
    Self(keys)
  }

  fn from_assignments<'a>(assignments: impl Iterator<Item = &'a ChannelRoleAssignment>) -> Self {
    Self(
      assignments
        .filter_map(|assignment| {
          let key = <[u8; 32]>::try_from(assignment.identity_key.as_slice()).ok()?;
          Some((
            convert::user(assignment.user_id),
            IdentityPublicKey::from_bytes(key),
          ))
        })
        .collect(),
    )
  }
}

impl IdentityDirectory for KeyDirectory {
  fn pinned_identity(&self, user_id: crypto::UserId) -> Option<IdentityPublicKey> {
    self.0.get(&user_id).copied()
  }
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair, SignedDeviceCertificate};
  use teamcraft_crypto::workspace::sign_log_entry;
  use teamcraft_proto::{Id, Snowflake, UnixMillis};

  use super::*;
  use crate::workspace::{LogHeads, RosterDocument, RosterMember};

  const CHANNEL: i64 = 900;
  const NOW_MS: i64 = 1_000_000;
  const MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;

  mod accepted {
    use super::*;

    #[test]
    fn the_founder_and_then_a_channel_admin_build_the_roles() {
      let (alice, bob, carol) = (Person::new(1), Person::new(2), Person::new(3));
      let world = World::new(&[&alice, &bob, &carol], &[]);
      let mut state = ChannelRoleState::default();
      let founding = alice.entry(&state, &[(&alice, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, founding).is_empty());
      assert_eq!(state.role_of(alice.user_id), ChannelRole::Admin);

      let promote = alice.entry(
        &state,
        &[
          (&alice, ChannelRole::Admin),
          (&bob, ChannelRole::Moderator),
          (&carol, ChannelRole::Admin),
        ],
      );
      assert!(world.apply(&mut state, promote).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Moderator);
      // Carol, made admin, signs the next entry herself.
      let demote = carol.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&carol, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, demote).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Member);
      assert_eq!((state.version, state.cursor), (3, 3));
    }

    #[test]
    fn a_workspace_admin_signs_any_snapshot_even_on_another_head() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let mut state = founded(&world, &alice, &[]);
      // Built on a head this device never accepted: a checkpoint that re-anchors it.
      let mut stale = state.clone();
      stale.maybe_head_hash = Some(Base64Bytes::new(vec![7; 32]));
      let checkpoint = admin.entry(&stale, &[(&bob, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, checkpoint).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Admin);
      assert_eq!(state.role_of(alice.user_id), ChannelRole::Member);
    }

    #[test]
    fn a_workspace_admin_founding_a_channel_signs_as_its_founder() {
      let admin = Person::new(9);
      let world = World::new(&[&admin], &[&admin]);
      let mut state = ChannelRoleState::default();
      let founding = admin.entry(&state, &[(&admin, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, founding).is_empty());
      assert_eq!(state.role_of(admin.user_id), ChannelRole::Admin);
    }
  }

  mod refused {
    use super::*;

    #[test]
    fn moderators_members_and_strangers_sign_nothing() {
      let (alice, bob, carol, stranger) = (Person::new(1), Person::new(2), Person::new(3), Person::new(4));
      let world = World::new(&[&alice, &bob, &carol, &stranger], &[]);
      let mut state = founded(&world, &alice, &[(&bob, ChannelRole::Moderator)]);
      for signer in [&bob, &carol, &stranger] {
        let entry = signer.entry(
          &state,
          &[(&alice, ChannelRole::Admin), (&carol, ChannelRole::Admin)],
        );
        let skipped = world.apply(&mut state, entry);
        assert!(
          matches!(skipped.as_slice(), [ChannelRoleError::Unauthorized { .. }]),
          "{skipped:?}"
        );
      }
      assert_eq!(state.role_of(carol.user_id), ChannelRole::Member);
      assert_eq!(state.version, 1);
    }

    #[test]
    fn unsigned_tampered_and_misplaced_entries_are_skipped() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let state = founded(&world, &alice, &[]);
      let changes = [(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)];

      let mut unsigned = alice.entry(&state, &changes);
      unsigned.signature = Base64Bytes::new(Vec::new());
      let mut tampered = alice.entry(&state, &changes);
      let mut document: ChannelRolesDocument = serde_json::from_slice(tampered.payload.as_slice()).unwrap();
      document
        .assignments
        .retain(|assignment| assignment.user_id != alice.user_id);
      tampered.payload = Base64Bytes::new(serde_json::to_vec(&document).unwrap());
      // Signed for the roster log, not for a channel's role log.
      let mut other_log = alice.entry(&state, &changes);
      other_log.signature = Base64Bytes::new(
        sign_log_entry(
          &alice.device,
          &alice.certificate,
          WorkspaceLog::Roster,
          other_log.version,
          other_log.payload.as_slice(),
        )
        .unwrap(),
      );
      for entry in [unsigned, tampered, other_log] {
        let mut probe = state.clone();
        assert_eq!(world.apply(&mut probe, entry).len(), 1);
        assert_eq!(probe.role_of(bob.user_id), ChannelRole::Member);
        assert_eq!(probe.version, 1);
      }
      // A valid entry of another channel is refused here.
      let entry = alice.entry_in(CHANNEL + 1, &state, &changes);
      let mut probe = state.clone();
      assert!(matches!(
        world.apply(&mut probe, entry).as_slice(),
        [ChannelRoleError::OtherChannel { .. }]
      ));
    }

    #[test]
    fn a_founder_claim_with_a_key_this_device_did_not_pin_is_refused() {
      // The server made up a key for Alice and signed with it: not the key pinned for her.
      let alice = Person::new(1);
      let impostor = Person::new(1);
      let world = World::new(&[&alice], &[]);
      let mut state = ChannelRoleState::default();
      let forged = impostor.entry(&state, &[(&impostor, ChannelRole::Admin)]);
      assert!(matches!(
        world.apply(&mut state, forged).as_slice(),
        [ChannelRoleError::Unauthorized { .. }]
      ));
      assert!(!state.is_signed());
    }

    #[test]
    fn nobody_founds_a_channel_twice() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[]);
      // Bob claims the channel as if nothing was signed.
      let unsigned = ChannelRoleState {
        cursor: state.cursor,
        ..ChannelRoleState::default()
      };
      let claim = bob.entry(&unsigned, &[(&bob, ChannelRole::Admin)]);
      assert_eq!(world.apply(&mut state, claim).len(), 1);
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Member);
    }

    #[test]
    fn role_rules_hold_for_channel_admins() {
      let (alice, bob, carol) = (Person::new(1), Person::new(2), Person::new(3));
      let world = World::new(&[&alice, &bob, &carol], &[]);
      let state = founded(&world, &alice, &[(&bob, ChannelRole::Admin)]);
      // Bob changes his own role.
      let own = bob.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Moderator)],
      );
      assert!(matches!(
        world.apply(&mut state.clone(), own).as_slice(),
        [ChannelRoleError::RoleRule {
          rule: RoleChangeError::OwnRole,
          ..
        }]
      ));
      // Nobody left as admin.
      let mut none = bob.document(&state, &[(&bob, ChannelRole::Admin)]);
      none.assignments.clear();
      assert!(matches!(
        world.apply(&mut state.clone(), bob.signed(none)).as_slice(),
        [ChannelRoleError::Snapshot { .. }]
      ));
      // A role this version doesn't know.
      let mut unknown = bob.document(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      unknown.assignments.push(ChannelRoleAssignment {
        user_id: carol.user_id,
        role: ChannelRole::Unknown("owner".into()),
        identity_key: carol.key(),
      });
      assert!(matches!(
        world.apply(&mut state.clone(), bob.signed(unknown)).as_slice(),
        [ChannelRoleError::Snapshot { .. }]
      ));
    }

    #[test]
    fn a_stale_roster_gives_workspace_admins_no_authority() {
      let (alice, admin) = (Person::new(1), Person::new(9));
      let mut world = World::new(&[&alice], &[&admin]);
      let mut state = founded(&world, &alice, &[]);
      world.workspace.heads.maybe_roster_signed_at_ms = Some(NOW_MS - MAX_AGE_MS - 1);
      let entry = admin.entry(&state, &[(&admin, ChannelRole::Admin)]);
      assert_eq!(world.apply(&mut state, entry).len(), 1);
      assert_eq!(state.role_of(admin.user_id), ChannelRole::Member);
    }
  }

  mod replay_and_rollback {
    use super::*;

    #[test]
    fn an_old_entry_again_or_at_another_version_changes_nothing() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[]);
      let promote = alice.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, promote.clone()).is_empty());
      let demote = alice.entry(&state, &[(&alice, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, demote).is_empty());
      // The server serves the promotion again: at or below the cursor, ignored.
      assert!(world.apply(&mut state, promote.clone()).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Member);
      // At a new position: the version is signed, so it doesn't verify there.
      let mut moved = promote;
      moved.version = state.cursor + 1;
      assert_eq!(world.apply(&mut state, moved).len(), 1);
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Member);
      assert_eq!(state.version, 3);
    }

    #[test]
    fn a_junk_entry_does_not_block_the_next_valid_one() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[]);
      let mut junk = bob.entry(&state, &[(&bob, ChannelRole::Admin)]);
      junk.payload = Base64Bytes::new(b"not json".to_vec());
      assert_eq!(world.apply(&mut state, junk).len(), 1);
      // Alice built on the last entry she accepted, the founding one.
      let valid = alice.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Moderator)],
      );
      assert_eq!(valid.version, 3);
      assert!(world.apply(&mut state, valid).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Moderator);
    }

    #[test]
    fn a_channel_admin_entry_built_on_another_head_is_refused() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[(&bob, ChannelRole::Admin)]);
      let mut forked = state.clone();
      forked.maybe_head_hash = Some(Base64Bytes::new(vec![5; 32]));
      let entry = bob.entry(
        &forked,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(matches!(
        world.apply(&mut state, entry).as_slice(),
        [ChannelRoleError::BrokenChain { .. }]
      ));
    }

    #[test]
    fn diverging_and_withheld_heads_raise_an_alarm() {
      let alice = Person::new(1);
      let world = World::new(&[&alice], &[]);
      let mut state = founded(&world, &alice, &[]);
      let own = state.head().unwrap();
      state.note_head(&own);
      assert_eq!(state.maybe_alarm, None);
      state.note_head(&ChannelRolesHead {
        version: own.version,
        hash: Base64Bytes::new(vec![1; 32]),
      });
      assert!(state.maybe_alarm.take().is_some(), "a fork");
      state.note_head(&ChannelRolesHead {
        version: own.version + 1,
        hash: Base64Bytes::new(vec![1; 32]),
      });
      assert!(state.maybe_alarm.is_none());
      state.check_ahead();
      assert!(state.maybe_alarm.is_some(), "entries this device isn't shown");
    }
  }

  mod legacy_and_effective_roles {
    use super::*;

    #[test]
    fn the_server_takes_roles_away_but_never_grants_them() {
      use ChannelRole::{Admin, Member, Moderator};
      for (verified, server, is_guest, effective) in [
        (Admin, Admin, false, Admin),
        (Admin, Member, false, Member),
        (Member, Admin, false, Member),
        (Moderator, Admin, false, Moderator),
        (Admin, Admin, true, Member),
      ] {
        assert_eq!(
          effective_role(&verified, &server, is_guest),
          effective,
          "{verified:?} / {server:?} / guest {is_guest}"
        );
      }
    }

    #[test]
    fn a_channel_without_a_signed_entry_grants_nobody_a_role() {
      let state = ChannelRoleState::default();
      assert!(!state.is_signed());
      assert_eq!(state.role_of(user(1)), ChannelRole::Member);
      assert_eq!(
        effective_role(&state.role_of(user(1)), &ChannelRole::Admin, false),
        ChannelRole::Member
      );
    }

    #[test]
    fn who_may_sign_follows_the_verified_roles() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let unsigned = ChannelRoleState::default();
      assert_eq!(
        unsigned.signing_authorities(bob.user_id, &world.workspace, NOW_MS, MAX_AGE_MS),
        vec![SignerAuthority::Founder]
      );
      let state = founded(&world, &alice, &[(&bob, ChannelRole::Moderator)]);
      assert!(
        state
          .signing_authorities(bob.user_id, &world.workspace, NOW_MS, MAX_AGE_MS)
          .is_empty()
      );
      assert_eq!(
        state.signing_authorities(alice.user_id, &world.workspace, NOW_MS, MAX_AGE_MS),
        vec![SignerAuthority::ChannelAdmin]
      );
      assert_eq!(
        state.signing_authorities(admin.user_id, &world.workspace, NOW_MS, MAX_AGE_MS),
        vec![SignerAuthority::Workspace { is_owner: false }]
      );
    }
  }

  struct Person {
    user_id: UserId,
    identity: IdentityKeyPair,
    device: DeviceKeyPair,
    certificate: SignedDeviceCertificate,
  }

  impl Person {
    fn new(raw: i64) -> Self {
      let user_id = user(raw);
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let device = DeviceKeyPair::generate(
        convert::user(user_id),
        crypto::DeviceId::new(raw.unsigned_abs() * 100),
        &mut OsRng,
      );
      let certificate = identity
        .certify_device(convert::user(user_id), &device.public_keys())
        .unwrap();
      Self {
        user_id,
        identity,
        device,
        certificate,
      }
    }

    fn key(&self) -> Base64Bytes {
      Base64Bytes::new(self.identity.public_key().as_bytes().to_vec())
    }

    /// The next document after `state`, holding exactly `roles`.
    fn document(&self, state: &ChannelRoleState, roles: &[(&Person, ChannelRole)]) -> ChannelRolesDocument {
      self.document_in(CHANNEL, state, roles)
    }

    fn document_in(
      &self,
      channel: i64,
      state: &ChannelRoleState,
      roles: &[(&Person, ChannelRole)],
    ) -> ChannelRolesDocument {
      let mut assignments: Vec<ChannelRoleAssignment> = roles
        .iter()
        .map(|(person, role)| ChannelRoleAssignment {
          user_id: person.user_id,
          role: role.clone(),
          identity_key: person.key(),
        })
        .collect();
      assignments.sort_by_key(|assignment| assignment.user_id);
      ChannelRolesDocument {
        version: state.cursor + 1,
        conversation_id: conversation(channel),
        assignments,
        prev_hash: state.head_or_zero(),
        signed_at_ms: NOW_MS,
      }
    }

    fn entry(
      &self,
      state: &ChannelRoleState,
      roles: &[(&Person, ChannelRole)],
    ) -> ChannelRoleLogEntryResponse {
      self.signed(self.document(state, roles))
    }

    fn entry_in(
      &self,
      channel: i64,
      state: &ChannelRoleState,
      roles: &[(&Person, ChannelRole)],
    ) -> ChannelRoleLogEntryResponse {
      self.signed(self.document_in(channel, state, roles))
    }

    fn signed(&self, document: ChannelRolesDocument) -> ChannelRoleLogEntryResponse {
      let payload = serde_json::to_vec(&document).unwrap();
      let signature = sign_log_entry(
        &self.device,
        &self.certificate,
        WorkspaceLog::ChannelRoles,
        document.version,
        &payload,
      )
      .unwrap();
      ChannelRoleLogEntryResponse {
        version: document.version,
        payload: Base64Bytes::new(payload),
        signature: Base64Bytes::new(signature),
        maybe_signer_device_id: None,
        created_at: UnixMillis(NOW_MS),
      }
    }
  }

  /// What a verifying device knows: the keys it pinned and the verified, fresh roster.
  struct World {
    pinned: KeyDirectory,
    workspace: WorkspaceState,
  }

  impl World {
    fn new(people: &[&Person], roster_admins: &[&Person]) -> Self {
      let pinned = KeyDirectory::from_keys(
        people
          .iter()
          .chain(roster_admins)
          .map(|person| (convert::user(person.user_id), person.identity.public_key()))
          .collect(),
      );
      let workspace = WorkspaceState {
        roster: RosterDocument {
          version: 1,
          members: roster_admins
            .iter()
            .map(|person| RosterMember {
              user_id: person.user_id,
              role: RosterRole::Admin,
              identity_key: person.key(),
            })
            .collect(),
          ..RosterDocument::default()
        },
        heads: LogHeads {
          is_roster_chained: true,
          maybe_roster_signed_at_ms: Some(NOW_MS),
          ..LogHeads::default()
        },
        ..WorkspaceState::default()
      };
      Self { pinned, workspace }
    }

    fn apply(
      &self,
      state: &mut ChannelRoleState,
      entry: ChannelRoleLogEntryResponse,
    ) -> Vec<ChannelRoleError> {
      let verifier = RoleVerifier {
        conversation: conversation(CHANNEL),
        workspace: &self.workspace,
        roster_max_age_ms: MAX_AGE_MS,
        pinned: &self.pinned,
      };
      state.apply_entries(vec![entry], &verifier)
    }
  }

  /// A channel founded by `founder`, with `others` given their roles in the founding entry.
  fn founded(world: &World, founder: &Person, others: &[(&Person, ChannelRole)]) -> ChannelRoleState {
    let mut state = ChannelRoleState::default();
    let mut roles = vec![(founder, ChannelRole::Admin)];
    roles.extend(others.iter().map(|(person, role)| (*person, role.clone())));
    let entry = founder.entry(&state, &roles);
    assert!(world.apply(&mut state, entry).is_empty());
    state
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn conversation(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
