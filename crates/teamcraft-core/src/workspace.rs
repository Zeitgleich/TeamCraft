//! The verified workspace state: admin roster and policy (admin.md §2.3, e2ee.md §11).
//!
//! The server stores both logs but makes no trust decision. The client verifies them as a chain:
//! the first roster entry names the first owner and is signed by them – that identity becomes the
//! workspace root of trust, pinned on first use. Every later roster or policy entry must be signed
//! by an owner of the roster verified up to that point. An entry that fails stops the chain; the
//! client keeps the last verified state and reports the rejection.
//!
//! Security review A6/A7: each signed document names the hash of the previous entry of its log
//! (`prev_hash`) and when it was signed (`signed_at_ms`); a policy names the roster version it was
//! signed under and is checked against the owners of exactly that roster. Rosters are applied
//! before policies, so the server's timestamps don't decide the order. Once a client has seen a
//! chained entry it requires the chain; entries from before (without the fields) still verify.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::ids as crypto;
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_crypto::workspace::{WorkspaceLog, log_entry_hash, verify_log_entry};
use teamcraft_proto::{
  Base64Bytes, ClientUpdatePolicy, UserId, WorkspaceLogEntryResponse, WorkspaceLogKind, WorkspacePolicy,
};

use crate::convert;
use crate::records;
use crate::store::{LocalStore, Table};

/// Admin tenures a device keeps. Far beyond any instance's turnover; past it the oldest go and the
/// history counts as incomplete, which fails closed (e2ee.md §4.6.1, "Reading the log late").
const MAX_ADMIN_TENURES: usize = 1024;

/// The roster document an owner signs. A full snapshot per version, so verifying needs no deltas.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterDocument {
  /// Equals the entry's version (replay protection, like the policy document).
  pub version: u64,
  pub members: Vec<RosterMember>,
  /// The organization recovery public key (e2ee.md §6.3.1), anchored here so the server can't
  /// swap it.
  #[serde(rename = "org_recovery_key", default, skip_serializing_if = "Option::is_none")]
  pub maybe_org_recovery_key: Option<Base64Bytes>,
  /// Hash of the previous roster entry ([`log_entry_hash`]).
  #[serde(rename = "prev_hash", default, skip_serializing_if = "Option::is_none")]
  pub maybe_prev_hash: Option<Base64Bytes>,
  /// When the owner signed it; owners re-sign regularly so clients can tell a stale roster.
  #[serde(rename = "signed_at_ms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_signed_at_ms: Option<i64>,
}

/// The policy document as this client signs and reads it: the wire [`PolicyDocument`] plus the
/// chain fields (the server ignores fields it doesn't know).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainedPolicyDocument {
  pub version: u64,
  pub policy: WorkspacePolicy,
  /// The roster version the signer was an owner in; 0 in documents from before the chain.
  #[serde(default)]
  pub roster_version: u64,
  #[serde(rename = "prev_hash", default, skip_serializing_if = "Option::is_none")]
  pub maybe_prev_hash: Option<Base64Bytes>,
  #[serde(rename = "signed_at_ms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_signed_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterMember {
  pub user_id: UserId,
  pub role: RosterRole,
  /// The member's identity public key: admins are verified against this, not trust on first use.
  pub identity_key: Base64Bytes,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RosterRole {
  Owner,
  Admin,
  #[serde(other)]
  Unknown,
}

/// What this client verified so far.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceState {
  /// The first owner, pinned when the genesis roster entry was verified.
  #[serde(rename = "root", default, skip_serializing_if = "Option::is_none")]
  pub maybe_root: Option<RootOfTrust>,
  #[serde(default)]
  pub roster: RosterDocument,
  #[serde(default)]
  pub policy_version: u64,
  #[serde(default)]
  pub policy: WorkspacePolicy,
  /// Why the chain stopped, if an entry was rejected.
  #[serde(rename = "rejection", default, skip_serializing_if = "Option::is_none")]
  pub maybe_rejection: Option<String>,
  #[serde(default)]
  pub heads: LogHeads,
  /// Owners of recent roster versions (from the last policy's roster version on), to verify
  /// policies against the roster they name.
  #[serde(default)]
  pub owners_by_version: BTreeMap<u64, BTreeMap<UserId, Base64Bytes>>,
  /// The roster version the last applied policy named.
  #[serde(default)]
  pub policy_roster_version: u64,
  /// A fork or withheld entries noticed by comparing heads with other members.
  #[serde(rename = "alarm", default, skip_serializing_if = "Option::is_none")]
  pub maybe_alarm: Option<String>,
  /// A member's head newer than ours, to check after the next sync.
  #[serde(rename = "ahead", default, skip_serializing_if = "Option::is_none")]
  pub maybe_ahead: Option<WorkspaceHead>,
  /// When each owner of the current roster last signed a verified roster or policy entry (the
  /// signing time the document names), for the admin console (issue #188).
  #[serde(default)]
  pub owner_signed_at_ms: BTreeMap<UserId, i64>,
  /// Who held admin authority on which verified roster versions, to judge a channel role entry by
  /// somebody who has left the roster since (e2ee.md §4.6.1, "Reading the log late").
  #[serde(default)]
  pub admin_history: AdminHistory,
}

/// Every stretch somebody was an owner or admin of the verified roster, from this device's own
/// verification of the signed roster log. Local state; nothing here is the server's word.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminHistory {
  /// Whether this goes back to the genesis roster. A state from before this was recorded has
  /// nothing until [`WorkspaceState::rebuild_admin_history`] replayed the log.
  #[serde(default)]
  pub is_complete: bool,
  #[serde(default)]
  pub tenures: Vec<AdminTenure>,
}

/// One stretch of one identity key on the roster: a new key, or coming back, is a new tenure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminTenure {
  pub user_id: UserId,
  pub identity_key: Base64Bytes,
  /// The role when the tenure last changed.
  pub is_owner: bool,
  /// When the owner signed the roster that began it; `None` for a roster without a signing time.
  #[serde(rename = "from_ms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_from_ms: Option<i64>,
  /// The first roster without them; `None` while they are on the roster.
  #[serde(rename = "until", default, skip_serializing_if = "Option::is_none")]
  pub maybe_until: Option<TenureEnd>,
}

/// The signed roster that ended a tenure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenureEnd {
  pub version: u64,
  /// When the owner signed it; `None` for a roster without a signing time, which proves no end.
  #[serde(rename = "at_ms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_at_ms: Option<i64>,
}

/// The owners of the verified roster, as [`WorkspaceState::owner_status`] reports them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerStatus {
  /// Each owner and the newest signing time of theirs this device verified.
  pub owners: Vec<(UserId, Option<i64>)>,
  /// Admins of the roster (not owners).
  pub admins: Vec<UserId>,
  /// Owners whose identity key in the roster can't be used: no signers, but still in the roster.
  pub unverifiable_owners: Vec<UserId>,
  /// When clients stop accepting the admins unless an owner signs the roster again; `None` for a
  /// roster from before signing times.
  pub maybe_expires_at_ms: Option<i64>,
  /// From the signed policy; at least 1.
  pub recommended_minimum: u8,
}

/// What a member's client has verified of both logs; sent along inside encrypted messages so
/// members notice a server showing them different logs (security review A6).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceHead {
  pub roster_version: u64,
  #[serde(rename = "roster_hash", default, skip_serializing_if = "Option::is_none")]
  pub maybe_roster_hash: Option<Base64Bytes>,
  pub policy_version: u64,
  #[serde(rename = "policy_hash", default, skip_serializing_if = "Option::is_none")]
  pub maybe_policy_hash: Option<Base64Bytes>,
}

/// The newest verified entry of each log, as members compare them (security review A6).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogHeads {
  #[serde(rename = "roster_hash", default, skip_serializing_if = "Option::is_none")]
  pub maybe_roster_hash: Option<Base64Bytes>,
  #[serde(rename = "policy_hash", default, skip_serializing_if = "Option::is_none")]
  pub maybe_policy_hash: Option<Base64Bytes>,
  /// Set once a chained roster entry was applied: later ones must chain too.
  #[serde(default)]
  pub is_roster_chained: bool,
  #[serde(default)]
  pub is_policy_chained: bool,
  #[serde(
    rename = "roster_signed_at_ms",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_roster_signed_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootOfTrust {
  pub user_id: UserId,
  pub identity_key: [u8; 32],
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
  #[error("{log} entry {version} is out of order (expected {expected})")]
  OutOfOrder {
    log: &'static str,
    version: u64,
    expected: u64,
  },
  #[error("{log} entry {version} has an unreadable payload: {reason}")]
  Payload {
    log: &'static str,
    version: u64,
    reason: String,
  },
  #[error("{log} entry {version} is not validly signed by an owner: {reason}")]
  Signature {
    log: &'static str,
    version: u64,
    reason: String,
  },
  #[error("the genesis roster's first owner differs from the pinned root of trust")]
  RootChanged,
  #[error("{log} entry {version} doesn't continue the chain this client verified")]
  BrokenChain { log: &'static str, version: u64 },
  #[error("policy entry {version} names roster version {roster_version}, which this client can't check")]
  StaleRosterVersion { version: u64, roster_version: u64 },
  /// Nobody could sign the next roster or policy: the workspace could never be administered again
  /// (admin.md §2.3).
  #[error("roster entry {version} has no owner")]
  NoOwner { version: u64 },
}

impl RosterDocument {
  /// Owners with a usable identity key: only they can sign the next entry.
  pub fn owner_count(&self) -> usize {
    identities(self, |role| *role == RosterRole::Owner).len()
  }
}

impl AdminHistory {
  /// Tenures of `user` that have ended, in the order they began.
  pub fn ended_tenures_of(&self, user: UserId) -> impl Iterator<Item = &AdminTenure> + '_ {
    self
      .tenures
      .iter()
      .filter(move |tenure| tenure.user_id == user && tenure.maybe_until.is_some())
  }

  /// Follows a verified roster: opens a tenure for each new owner or admin and ends the ones it
  /// no longer lists. Does nothing while the history doesn't go back to the genesis roster: a
  /// departure this device didn't follow can't be told from somebody who never was an admin.
  fn record(&mut self, roster: &RosterDocument) {
    if !self.is_complete {
      return;
    }
    for tenure in &mut self.tenures {
      if tenure.maybe_until.is_some() {
        continue;
      }
      let maybe_member = roster.members.iter().find(|member| {
        member.user_id == tenure.user_id
          && member.identity_key == tenure.identity_key
          && matches!(member.role, RosterRole::Owner | RosterRole::Admin)
      });
      match maybe_member {
        Some(member) => tenure.is_owner = member.role == RosterRole::Owner,
        None => {
          tenure.maybe_until = Some(TenureEnd {
            version: roster.version,
            maybe_at_ms: roster.maybe_signed_at_ms,
          });
        },
      }
    }
    for member in &roster.members {
      let is_admin = matches!(member.role, RosterRole::Owner | RosterRole::Admin);
      let is_open = self.tenures.iter().any(|tenure| {
        tenure.maybe_until.is_none()
          && tenure.user_id == member.user_id
          && tenure.identity_key == member.identity_key
      });
      if is_admin && !is_open {
        self.tenures.push(AdminTenure {
          user_id: member.user_id,
          identity_key: member.identity_key.clone(),
          is_owner: member.role == RosterRole::Owner,
          maybe_from_ms: roster.maybe_signed_at_ms,
          maybe_until: None,
        });
      }
    }
    if self.tenures.len() > MAX_ADMIN_TENURES {
      self.is_complete = false;
      let excess = self.tenures.len() - MAX_ADMIN_TENURES;
      self.tenures.drain(..excess);
    }
  }
}

impl WorkspaceState {
  /// Users with admin rights (owners and admins) of the verified roster.
  pub fn admins(&self) -> impl Iterator<Item = UserId> + '_ {
    self
      .roster
      .members
      .iter()
      .filter(|member| matches!(member.role, RosterRole::Owner | RosterRole::Admin))
      .map(|member| member.user_id)
  }

  pub fn is_admin(&self, user: UserId) -> bool {
    self.admins().any(|admin| admin == user)
  }

  /// Identity keys the roster vouches for (owners and admins).
  pub fn roster_identities(&self) -> BTreeMap<UserId, [u8; 32]> {
    identities(&self.roster, |_| true)
  }

  /// Whether admin authority still holds: once rosters carry a signing time, the newest one must
  /// be younger than `max_age_ms`, so a server that withholds a demotion only buys that long
  /// (security review A6). Owners' clients re-sign before that.
  pub fn is_roster_fresh(&self, now_ms: i64, max_age_ms: i64) -> bool {
    match self.heads.maybe_roster_signed_at_ms {
      Some(signed_at) => now_ms.saturating_sub(signed_at) <= max_age_ms,
      None => !self.heads.is_roster_chained,
    }
  }

  pub fn head(&self) -> WorkspaceHead {
    WorkspaceHead {
      roster_version: self.roster.version,
      maybe_roster_hash: self.heads.maybe_roster_hash.clone(),
      policy_version: self.policy_version,
      maybe_policy_hash: self.heads.maybe_policy_hash.clone(),
    }
  }

  /// Compares another member's head with ours: the same version with another hash is a fork (two
  /// different entries were shown); a newer version is remembered until the next sync shows it.
  pub fn note_head(&mut self, other: &WorkspaceHead) {
    let own = self.head();
    let is_fork = (other.roster_version == own.roster_version
      && other.maybe_roster_hash.is_some()
      && own.maybe_roster_hash.is_some()
      && other.maybe_roster_hash != own.maybe_roster_hash)
      || (other.policy_version == own.policy_version
        && other.maybe_policy_hash.is_some()
        && own.maybe_policy_hash.is_some()
        && other.maybe_policy_hash != own.maybe_policy_hash);
    if is_fork {
      self.maybe_alarm = Some(format!(
        "another member verified a different workspace log (roster {}, policy {}): the server shows members different logs",
        other.roster_version, other.policy_version
      ));
    } else if other.roster_version > own.roster_version || other.policy_version > own.policy_version {
      self.maybe_ahead = Some(other.clone());
    }
  }

  /// After a sync: a member's newer head that this client still can't see means the server is
  /// holding entries back from it.
  pub fn check_ahead(&mut self) {
    let Some(ahead) = self.maybe_ahead.take() else {
      return;
    };
    if ahead.roster_version > self.roster.version || ahead.policy_version > self.policy_version {
      self.maybe_alarm = Some(format!(
        "another member has workspace log entries this device isn't shown (roster {}, policy {})",
        ahead.roster_version, ahead.policy_version
      ));
    } else {
      self.note_head(&ahead);
    }
  }

  /// Verifies and applies new log entries: all roster entries first, then the policy entries, each
  /// in version order. Stops at the first invalid entry and records why.
  pub fn apply_entries(&mut self, entries: Vec<WorkspaceLogEntryResponse>) -> Result<(), WorkspaceError> {
    let (mut rosters, mut policies): (Vec<_>, Vec<_>) = entries
      .into_iter()
      .filter(|entry| !matches!(entry.kind, WorkspaceLogKind::Unknown(_)))
      .partition(|entry| entry.kind == WorkspaceLogKind::Roster);
    rosters.sort_by_key(|entry| entry.version);
    policies.sort_by_key(|entry| entry.version);
    let result = rosters
      .iter()
      .try_for_each(|entry| self.apply_roster(entry))
      .and_then(|()| policies.iter().try_for_each(|entry| self.apply_policy(entry)));
    if let Err(error) = &result {
      self.maybe_rejection = Some(error.to_string());
    }
    result
  }

  /// Fills in the admin history of a state from before it was recorded, by verifying the whole
  /// roster log again from the root of trust. Taken only if that ends on the head this device
  /// verified, so a server can't make up a past; otherwise the history stays incomplete and
  /// this returns `false`.
  pub fn rebuild_admin_history(&mut self, entries: Vec<WorkspaceLogEntryResponse>) -> bool {
    let up_to = self.roster.version;
    let mut replay = Self {
      maybe_root: self.maybe_root.clone(),
      ..Self::default()
    };
    let rosters = entries
      .into_iter()
      .filter(|entry| entry.kind == WorkspaceLogKind::Roster && entry.version <= up_to)
      .collect();
    let is_same_log = replay.apply_entries(rosters).is_ok()
      && replay.roster.version == up_to
      && replay.heads.maybe_roster_hash == self.heads.maybe_roster_hash;
    if is_same_log {
      self.admin_history = replay.admin_history;
    }
    is_same_log
  }

  fn apply_roster(&mut self, entry: &WorkspaceLogEntryResponse) -> Result<(), WorkspaceError> {
    let log = "roster";
    if entry.version <= self.roster.version {
      return Ok(());
    }
    expect_next(log, entry.version, self.roster.version)?;
    let document: RosterDocument = parse(log, entry)?;
    if document.version != entry.version {
      return Err(payload_error(
        log,
        entry,
        "document version differs from the entry",
      ));
    }
    // Refused whoever signed it: an owner's client never signs one, and a server that stores one
    // anyway must not be able to lock the workspace.
    if document.owner_count() == 0 {
      return Err(WorkspaceError::NoOwner {
        version: entry.version,
      });
    }
    check_chain(
      log,
      entry.version,
      document.maybe_prev_hash.as_ref(),
      self.heads.maybe_roster_hash.as_ref(),
      self.heads.is_roster_chained,
    )?;
    let is_genesis = self.roster.version == 0;
    // The genesis entry vouches for itself; later ones need an owner of the previous roster.
    let owners = if is_genesis {
      owner_directory(&document)
    } else {
      owner_directory(&self.roster)
    };
    let signer = verify(log, WorkspaceLog::Roster, entry, &owners)?;
    if is_genesis {
      let identity_key = *signer.1.as_bytes();
      let root = RootOfTrust {
        user_id: signer.0,
        identity_key,
      };
      if self.maybe_root.as_ref().is_some_and(|pinned| *pinned != root) {
        return Err(WorkspaceError::RootChanged);
      }
      self.maybe_root = Some(root);
    }
    self.heads.maybe_roster_hash = Some(entry_hash(log, WorkspaceLog::Roster, entry)?);
    self.admin_history.is_complete |= is_genesis;
    self.admin_history.record(&document);
    self.heads.is_roster_chained |= document.maybe_prev_hash.is_some();
    if document.maybe_signed_at_ms.is_some() {
      self.heads.maybe_roster_signed_at_ms = document.maybe_signed_at_ms;
    }
    self
      .owners_by_version
      .insert(document.version, owner_keys(&document));
    self.note_signature(signer.0, document.maybe_signed_at_ms);
    let owners = owner_keys(&document);
    self
      .owner_signed_at_ms
      .retain(|user, _| owners.contains_key(user));
    self.roster = document;
    Ok(())
  }

  fn apply_policy(&mut self, entry: &WorkspaceLogEntryResponse) -> Result<(), WorkspaceError> {
    let log = "policy";
    if entry.version <= self.policy_version {
      return Ok(());
    }
    expect_next(log, entry.version, self.policy_version)?;
    let document: ChainedPolicyDocument = parse(log, entry)?;
    if document.version != entry.version {
      return Err(payload_error(
        log,
        entry,
        "document version differs from the entry",
      ));
    }
    check_chain(
      log,
      entry.version,
      document.maybe_prev_hash.as_ref(),
      self.heads.maybe_policy_hash.as_ref(),
      self.heads.is_policy_chained,
    )?;
    let owners = self.policy_signers(&document)?;
    let signer = verify(log, WorkspaceLog::Policy, entry, &owners)?;
    if self
      .roster
      .members
      .iter()
      .any(|member| member.user_id == signer.0 && member.role == RosterRole::Owner)
    {
      self.note_signature(signer.0, document.maybe_signed_at_ms);
    }
    self.heads.maybe_policy_hash = Some(entry_hash(log, WorkspaceLog::Policy, entry)?);
    self.heads.is_policy_chained |= document.maybe_prev_hash.is_some();
    if document.roster_version > 0 {
      self.policy_roster_version = document.roster_version;
      let floor = document.roster_version;
      self.owners_by_version.retain(|version, _| *version >= floor);
    }
    self.policy_version = entry.version;
    self.policy = document.policy;
    Ok(())
  }

  /// Remembers the newest signing time of an owner; documents without one tell nothing.
  fn note_signature(&mut self, user: UserId, maybe_signed_at_ms: Option<i64>) {
    let Some(signed_at) = maybe_signed_at_ms else {
      return;
    };
    let latest = self.owner_signed_at_ms.entry(user).or_insert(signed_at);
    *latest = (*latest).max(signed_at);
  }

  /// Owners of the verified roster and when admin authority expires, for the admin console.
  /// `None` before this device verified a roster.
  pub fn owner_status(&self, max_age_ms: i64) -> Option<OwnerStatus> {
    if self.roster.version == 0 {
      return None;
    }
    let owners = owner_keys(&self.roster)
      .into_keys()
      .map(|user| (user, self.owner_signed_at_ms.get(&user).copied()))
      .collect();
    let maybe_expires_at_ms = match self.heads.maybe_roster_signed_at_ms {
      Some(signed_at) => Some(signed_at.saturating_add(max_age_ms)),
      // A chained roster without a signing time is never fresh (see `is_roster_fresh`).
      None if self.heads.is_roster_chained => Some(0),
      None => None,
    };
    let admins = self
      .roster
      .members
      .iter()
      .filter(|member| member.role == RosterRole::Admin)
      .map(|member| member.user_id)
      .collect();
    let unverifiable_owners = unusable_key_members(&self.roster)
      .into_iter()
      .filter(|(_, role)| *role == RosterRole::Owner)
      .map(|(user, _)| user)
      .collect();
    Some(OwnerStatus {
      owners,
      admins,
      unverifiable_owners,
      maybe_expires_at_ms,
      recommended_minimum: self.policy.owners.recommended_minimum.max(1),
    })
  }

  /// Owners who may sign this policy: those of the roster version it names, which must not be
  /// older than the one the previous policy named (so a removed owner can't sign a "back-dated"
  /// policy once a later one exists) nor newer than this client's roster.
  fn policy_signers(&self, document: &ChainedPolicyDocument) -> Result<RosterDirectory, WorkspaceError> {
    if document.roster_version == 0 {
      // From before the chain: checked against the current roster, as it was then.
      if self.heads.is_policy_chained {
        return Err(WorkspaceError::StaleRosterVersion {
          version: document.version,
          roster_version: 0,
        });
      }
      return Ok(owner_directory(&self.roster));
    }
    let stale = || WorkspaceError::StaleRosterVersion {
      version: document.version,
      roster_version: document.roster_version,
    };
    if document.roster_version < self.policy_roster_version || document.roster_version > self.roster.version {
      return Err(stale());
    }
    let owners = self
      .owners_by_version
      .get(&document.roster_version)
      .ok_or_else(stale)?;
    Ok(RosterDirectory(
      owners
        .iter()
        .filter_map(|(user, key)| {
          Some((
            convert::user(*user),
            IdentityPublicKey::from_bytes(<[u8; 32]>::try_from(key.as_slice()).ok()?),
          ))
        })
        .collect(),
    ))
  }
}

/// Roster members whose identity key isn't a usable key, with their role.
fn unusable_key_members(roster: &RosterDocument) -> Vec<(UserId, RosterRole)> {
  roster
    .members
    .iter()
    .filter(|member| <[u8; 32]>::try_from(member.identity_key.as_slice()).is_err())
    .map(|member| (member.user_id, member.role.clone()))
    .collect()
}

/// The first entry of `roles` that the verified roster holds with a key this device can't use: it
/// can't be signed again as it is, so the new roster is refused instead of changing it silently.
/// Leaving the person out of `roles` (an explicit removal) is fine.
pub fn kept_unusable_entry(roster: &RosterDocument, roles: &[(UserId, RosterRole)]) -> Option<UserId> {
  let unusable = unusable_key_members(roster);
  roles
    .iter()
    .map(|(user, _)| *user)
    .find(|user| unusable.iter().any(|(listed, _)| listed == user))
}

fn owner_keys(roster: &RosterDocument) -> BTreeMap<UserId, Base64Bytes> {
  identities(roster, |role| *role == RosterRole::Owner)
    .into_iter()
    .map(|(user, key)| (user, Base64Bytes::new(key.to_vec())))
    .collect()
}

/// A chained document must name the head this client has; an unchained one is accepted only
/// before the log's first chained entry.
fn check_chain(
  log: &'static str,
  version: u64,
  maybe_prev_hash: Option<&Base64Bytes>,
  maybe_head: Option<&Base64Bytes>,
  is_chained: bool,
) -> Result<(), WorkspaceError> {
  let is_valid = match maybe_prev_hash {
    Some(prev) => maybe_head.map_or(prev.as_slice().iter().all(|byte| *byte == 0), |head| prev == head),
    None => !is_chained,
  };
  if is_valid {
    Ok(())
  } else {
    Err(WorkspaceError::BrokenChain { log, version })
  }
}

fn entry_hash(
  log: &'static str,
  kind: WorkspaceLog,
  entry: &WorkspaceLogEntryResponse,
) -> Result<Base64Bytes, WorkspaceError> {
  log_entry_hash(
    kind,
    entry.version,
    entry.payload.as_slice(),
    entry.signature.as_slice(),
  )
  .map(|hash| Base64Bytes::new(hash.to_vec()))
  .map_err(|error| payload_error(log, entry, &error.to_string()))
}

/// Identity keys of a roster's members, as an [`IdentityDirectory`].
pub(crate) struct RosterDirectory(BTreeMap<crypto::UserId, IdentityPublicKey>);

impl IdentityDirectory for RosterDirectory {
  fn pinned_identity(&self, user_id: crypto::UserId) -> Option<IdentityPublicKey> {
    self.0.get(&user_id).copied()
  }
}

fn owner_directory(roster: &RosterDocument) -> RosterDirectory {
  RosterDirectory(
    identities(roster, |role| *role == RosterRole::Owner)
      .into_iter()
      .map(|(user, key)| (convert::user(user), IdentityPublicKey::from_bytes(key)))
      .collect(),
  )
}

fn identities(roster: &RosterDocument, include: impl Fn(&RosterRole) -> bool) -> BTreeMap<UserId, [u8; 32]> {
  roster
    .members
    .iter()
    .filter(|member| include(&member.role))
    .filter_map(|member| {
      Some((
        member.user_id,
        <[u8; 32]>::try_from(member.identity_key.as_slice()).ok()?,
      ))
    })
    .collect()
}

fn verify(
  log: &'static str,
  kind: WorkspaceLog,
  entry: &WorkspaceLogEntryResponse,
  owners: &RosterDirectory,
) -> Result<(UserId, IdentityPublicKey), WorkspaceError> {
  let device = verify_log_entry(
    entry.signature.as_slice(),
    kind,
    entry.version,
    entry.payload.as_slice(),
    owners,
  )
  .map_err(|error| WorkspaceError::Signature {
    log,
    version: entry.version,
    reason: error.to_string(),
  })?;
  let user = convert::user_from_crypto(device.user_id()).map_err(|error| WorkspaceError::Signature {
    log,
    version: entry.version,
    reason: error.to_string(),
  })?;
  let identity = owners
    .pinned_identity(device.user_id())
    .ok_or(WorkspaceError::Signature {
      log,
      version: entry.version,
      reason: "signer vanished".into(),
    })?;
  Ok((user, identity))
}

fn expect_next(log: &'static str, version: u64, current: u64) -> Result<(), WorkspaceError> {
  if version == current + 1 {
    Ok(())
  } else {
    Err(WorkspaceError::OutOfOrder {
      log,
      version,
      expected: current + 1,
    })
  }
}

fn parse<T: serde::de::DeserializeOwned>(
  log: &'static str,
  entry: &WorkspaceLogEntryResponse,
) -> Result<T, WorkspaceError> {
  serde_json::from_slice(entry.payload.as_slice())
    .map_err(|error| payload_error(log, entry, &error.to_string()))
}

fn payload_error(log: &'static str, entry: &WorkspaceLogEntryResponse, reason: &str) -> WorkspaceError {
  WorkspaceError::Payload {
    log,
    version: entry.version,
    reason: reason.to_string(),
  }
}

/// Whether the verified policy lets members set a recovery passphrase (e2ee.md §6.5.8). Before a
/// policy was verified (or when the state can't be read) the default applies, which allows it:
/// the server refuses new passphrases under a stricter policy either way.
pub fn is_passphrase_allowed(store: &impl LocalStore) -> bool {
  records::get_json::<WorkspaceState>(store, Table::Workspace, crate::client::WORKSPACE_KEY)
    .ok()
    .flatten()
    .is_none_or(|state| state.policy.recovery.is_passphrase_allowed)
}

/// How large screen shares may be under the verified policy (#565), for the share options.
/// `None` before a workspace was verified or when the state can't be read: the default (4K
/// offered), as for a policy signed before the setting.
pub fn screen_share_policy(store: &impl LocalStore) -> Option<teamcraft_proto::ScreenSharePolicy> {
  records::get_json::<WorkspaceState>(store, Table::Workspace, crate::client::WORKSPACE_KEY)
    .ok()
    .flatten()
    .map(|state| state.policy.screen_share)
}

/// Which statuses the verified policy allows, for the UI's status menu and the people it shows.
/// `None` before a workspace was verified or when the state can't be read; the server enforces
/// the policy either way, so this only decides what the UI offers.
pub fn presence_policy(store: &impl LocalStore) -> Option<teamcraft_proto::PresencePolicy> {
  records::get_json::<WorkspaceState>(store, Table::Workspace, crate::client::WORKSPACE_KEY)
    .ok()
    .flatten()
    .map(|state| state.policy.presence)
}

/// The owners of the verified roster for the admin console (`None` before one was verified, or
/// when the state can't be read).
#[cfg(feature = "ui")]
pub fn owner_roster_status(
  store: &impl LocalStore,
  max_age: std::time::Duration,
  warning_lead: std::time::Duration,
) -> Option<teamcraft_client_api::admin::OwnerRosterStatus> {
  use teamcraft_client_api::admin::{OwnerRosterStatus, OwnerSigning};

  let millis = |duration: std::time::Duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
  let state = records::get_json::<WorkspaceState>(store, Table::Workspace, crate::client::WORKSPACE_KEY)
    .ok()
    .flatten()?;
  let status = state.owner_status(millis(max_age))?;
  Some(OwnerRosterStatus {
    version: state.roster.version,
    owners: status
      .owners
      .into_iter()
      .map(|(user, maybe_last_signed_at)| OwnerSigning {
        user,
        maybe_last_signed_at,
      })
      .collect(),
    admins: status.admins,
    unverifiable_owners: status.unverifiable_owners,
    maybe_expires_at: status.maybe_expires_at_ms,
    recommended_minimum: status.recommended_minimum,
    warning_lead_ms: millis(warning_lead),
  })
}

/// The first roster's roles: the signer as owner (the root of trust, e2ee.md §11), then the owners
/// and admins the user confirmed, each once; an owner listed as admin too stays owner.
pub fn genesis_roles(me: UserId, owners: &[UserId], admins: &[UserId]) -> Vec<(UserId, RosterRole)> {
  let mut roles: Vec<(UserId, RosterRole)> = vec![(me, RosterRole::Owner)];
  for (user, role) in owners
    .iter()
    .map(|user| (*user, RosterRole::Owner))
    .chain(admins.iter().map(|user| (*user, RosterRole::Admin)))
  {
    if !roles.iter().any(|(listed, _)| *listed == user) {
      roles.push((user, role));
    }
  }
  roles
}

/// The core's roster roles for a roster the console proposes.
#[cfg(feature = "ui")]
pub fn roster_roles(members: &[teamcraft_client_api::admin::RosterEntry]) -> Vec<(UserId, RosterRole)> {
  use teamcraft_client_api::admin::RosterRoleChoice;

  members
    .iter()
    .map(|entry| {
      let role = match entry.role {
        RosterRoleChoice::Owner => RosterRole::Owner,
        RosterRoleChoice::Admin => RosterRole::Admin,
      };
      (entry.user, role)
    })
    .collect()
}

/// The update rules of the verified workspace policy (docs/updates.md §4.4); `None` while this
/// device has verified nothing yet (the desktop shell then keeps the last rules it saw).
pub fn verified_client_update_policy(store: &impl LocalStore) -> Option<ClientUpdatePolicy> {
  records::get_json::<WorkspaceState>(store, Table::Workspace, crate::client::WORKSPACE_KEY)
    .ok()
    .flatten()
    .map(|state| state.policy.client_updates)
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair, SignedDeviceCertificate};
  use teamcraft_crypto::workspace::sign_log_entry;
  use teamcraft_proto::{Id, Snowflake, UnixMillis};

  use super::*;

  #[test]
  fn the_genesis_roles_keep_the_signer_as_owner() {
    let user = |raw: i64| UserId::from_snowflake(Snowflake::new(raw).unwrap());
    let roles = genesis_roles(user(1), &[user(2), user(1)], &[user(1), user(2), user(3)]);
    assert_eq!(
      roles,
      vec![
        (user(1), RosterRole::Owner),
        (user(2), RosterRole::Owner),
        (user(3), RosterRole::Admin),
      ]
    );
  }

  #[test]
  fn a_removed_owner_cannot_backdate_a_policy() {
    // Security review A7: Alice is removed as owner in roster 2; she signs policy 3 naming
    // roster 1, when she still was an owner.
    let (alice, bob) = (Owner::new(1), Owner::new(2));
    let mut state = WorkspaceState::default();
    let roster_1 = roster(1, &[&alice, &bob], None, &state);
    let entry_1 = alice.sign(WorkspaceLogKind::Roster, 1, &roster_1);
    state.apply_entries(vec![entry_1]).unwrap();
    let policy_1 = policy(1, 1, &state);
    state
      .apply_entries(vec![alice.sign(WorkspaceLogKind::Policy, 1, &policy_1)])
      .unwrap();
    let roster_2 = roster(2, &[&bob], None, &state);
    state
      .apply_entries(vec![bob.sign(WorkspaceLogKind::Roster, 2, &roster_2)])
      .unwrap();
    // Bob's client signs the policy again under roster 2 (what `append_roster` does).
    let policy_2 = policy(2, 2, &state);
    state
      .apply_entries(vec![bob.sign(WorkspaceLogKind::Policy, 2, &policy_2)])
      .unwrap();

    let backdated = policy(3, 1, &state);
    assert!(matches!(
      state.apply_entries(vec![alice.sign(WorkspaceLogKind::Policy, 3, &backdated)]),
      Err(WorkspaceError::StaleRosterVersion { .. })
    ));
    let current = policy(3, 2, &state);
    assert!(matches!(
      state.apply_entries(vec![alice.sign(WorkspaceLogKind::Policy, 3, &current)]),
      Err(WorkspaceError::Signature { .. })
    ));
  }

  #[test]
  fn entries_must_continue_the_chain() {
    let alice = Owner::new(1);
    let mut state = WorkspaceState::default();
    let first = roster(1, &[&alice], None, &state);
    state
      .apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 1, &first)])
      .unwrap();
    let mut wrong = roster(2, &[&alice], None, &state);
    wrong.maybe_prev_hash = Some(Base64Bytes::new(vec![7; 32]));
    assert!(matches!(
      state.apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 2, &wrong)]),
      Err(WorkspaceError::BrokenChain { .. })
    ));
    let mut unchained = roster(2, &[&alice], None, &state);
    unchained.maybe_prev_hash = None;
    assert!(matches!(
      state.apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 2, &unchained)]),
      Err(WorkspaceError::BrokenChain { .. })
    ));
  }

  #[test]
  fn stale_rosters_and_diverging_heads_are_noticed() {
    let alice = Owner::new(1);
    let mut state = WorkspaceState::default();
    let first = roster(1, &[&alice], Some(1_000), &state);
    state
      .apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 1, &first)])
      .unwrap();
    assert!(state.is_roster_fresh(1_000 + 10, 100));
    assert!(!state.is_roster_fresh(1_000 + 101, 100));

    let mut other = state.head();
    other.maybe_roster_hash = Some(Base64Bytes::new(vec![9; 32]));
    state.note_head(&other);
    assert!(state.maybe_alarm.is_some(), "same version, other entry: a fork");

    let mut fresh = WorkspaceState::default();
    fresh
      .apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 1, &first)])
      .unwrap();
    let ahead = WorkspaceHead {
      roster_version: 5,
      ..fresh.head()
    };
    fresh.note_head(&ahead);
    fresh.check_ahead();
    assert!(
      fresh.maybe_alarm.is_some(),
      "a colleague sees entries this device isn't shown"
    );
  }

  mod last_owner {
    use super::*;

    #[test]
    fn a_validly_signed_roster_without_an_owner_is_refused() {
      // admin.md §2.3: an owner's (older or compromised) client signed it and the server stored it.
      let (alice, bob) = (Owner::new(1), Owner::new(2));
      let mut state = WorkspaceState::default();
      let first = roster(1, &[&alice], Some(1_000), &state);
      state
        .apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 1, &first)])
        .unwrap();

      let mut orphaned = roster(2, &[&alice, &bob], Some(2_000), &state);
      for member in &mut orphaned.members {
        member.role = RosterRole::Admin;
      }
      let refused = state.apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 2, &orphaned)]);
      assert!(matches!(refused, Err(WorkspaceError::NoOwner { version: 2 })));
      assert_eq!(state.roster.version, 1, "the last verified roster stays in force");
      assert!(state.is_admin(alice.user_id));
      assert!(state.maybe_rejection.is_some());

      let mut empty = roster(2, &[], Some(2_000), &state);
      empty.members.clear();
      assert!(matches!(
        state.apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 2, &empty)]),
        Err(WorkspaceError::NoOwner { version: 2 })
      ));
    }

    #[test]
    fn a_genesis_roster_without_an_owner_is_refused() {
      let alice = Owner::new(1);
      let mut state = WorkspaceState::default();
      let mut genesis = roster(1, &[&alice], None, &state);
      genesis.members[0].role = RosterRole::Admin;
      assert!(matches!(
        state.apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 1, &genesis)]),
        Err(WorkspaceError::NoOwner { version: 1 })
      ));
      assert_eq!(state.maybe_root, None, "no root of trust is pinned");
    }

    #[test]
    fn an_owner_without_a_usable_key_does_not_count() {
      let alice = Owner::new(1);
      let mut state = WorkspaceState::default();
      let first = roster(1, &[&alice], None, &state);
      state
        .apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 1, &first)])
        .unwrap();
      let mut broken = roster(2, &[&alice], None, &state);
      broken.members[0].identity_key = Base64Bytes::new(vec![1; 5]);
      assert_eq!(broken.owner_count(), 0);
      assert!(matches!(
        state.apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 2, &broken)]),
        Err(WorkspaceError::NoOwner { .. })
      ));
    }
  }

  mod admin_history {
    use super::*;

    /// Roster versions 1 to 3: Alice alone, with Bob, alone again.
    fn three_versions(alice: &Owner, bob: &Owner) -> (WorkspaceState, Vec<WorkspaceLogEntryResponse>) {
      let mut state = WorkspaceState::default();
      let mut entries = Vec::new();
      let versions: [(&[&Owner], i64); 3] = [(&[alice], 1_000), (&[alice, bob], 2_000), (&[alice], 3_000)];
      for (index, (owners, signed_at)) in versions.into_iter().enumerate() {
        let version = u64::try_from(index).unwrap() + 1;
        let document = roster(version, owners, Some(signed_at), &state);
        let entry = alice.sign(WorkspaceLogKind::Roster, version, &document);
        state.apply_entries(vec![entry.clone()]).unwrap();
        entries.push(entry);
      }
      (state, entries)
    }

    #[test]
    fn a_tenure_runs_from_the_roster_that_lists_somebody_to_the_first_that_does_not() {
      let (alice, bob) = (Owner::new(1), Owner::new(2));
      let (state, _) = three_versions(&alice, &bob);
      assert!(state.admin_history.is_complete);
      let ended: Vec<_> = state.admin_history.ended_tenures_of(bob.user_id).collect();
      assert_eq!(ended.len(), 1);
      assert_eq!(ended[0].maybe_from_ms, Some(2_000));
      assert_eq!(
        ended[0].maybe_until,
        Some(TenureEnd {
          version: 3,
          maybe_at_ms: Some(3_000),
        })
      );
      assert_eq!(state.admin_history.ended_tenures_of(alice.user_id).count(), 0);
    }

    #[test]
    fn a_new_identity_key_ends_the_tenure_of_the_old_one() {
      let (alice, bob, new_bob) = (Owner::new(1), Owner::new(2), Owner::new(2));
      let mut state = WorkspaceState::default();
      let first = roster(1, &[&alice, &bob], Some(1_000), &state);
      state
        .apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 1, &first)])
        .unwrap();
      let second = roster(2, &[&alice, &new_bob], Some(2_000), &state);
      state
        .apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 2, &second)])
        .unwrap();
      let ended: Vec<_> = state.admin_history.ended_tenures_of(bob.user_id).collect();
      assert_eq!(ended.len(), 1);
      assert_eq!(
        ended[0].identity_key.as_slice(),
        bob.identity.public_key().as_bytes()
      );
    }

    #[test]
    fn a_state_from_before_the_history_records_nothing_until_it_replayed_the_log() {
      let (alice, bob) = (Owner::new(1), Owner::new(2));
      let (complete, entries) = three_versions(&alice, &bob);
      let json = serde_json::to_value(&complete).unwrap();
      let mut older: WorkspaceState = serde_json::from_value(json).unwrap();
      older.admin_history = AdminHistory::default();

      assert!(older.rebuild_admin_history(entries));
      assert_eq!(older.admin_history, complete.admin_history);
    }

    #[test]
    fn a_log_that_does_not_end_on_the_verified_head_leaves_the_history_incomplete() {
      let (alice, bob) = (Owner::new(1), Owner::new(2));
      let (mut state, entries) = three_versions(&alice, &bob);
      state.admin_history = AdminHistory::default();

      // Too short, and from another log.
      assert!(!state.rebuild_admin_history(entries[..2].to_vec()));
      let (_, other) = three_versions(&alice, &Owner::new(3));
      assert!(!state.rebuild_admin_history(other));
      // Signed by somebody else: the root of trust doesn't take it.
      let stranger = Owner::new(4);
      let (_, forged) = three_versions(&stranger, &bob);
      assert!(!state.rebuild_admin_history(forged));
      assert_eq!(state.admin_history, AdminHistory::default());
    }

    #[test]
    fn past_the_bound_the_oldest_go_and_the_history_counts_as_incomplete() {
      let mut history = AdminHistory {
        is_complete: true,
        ..AdminHistory::default()
      };
      for version in 1..=(MAX_ADMIN_TENURES + 1) {
        let version = u64::try_from(version).unwrap();
        history.record(&RosterDocument {
          version,
          members: vec![RosterMember {
            user_id: UserId::from_snowflake(Snowflake::new(i64::try_from(version).unwrap()).unwrap()),
            role: RosterRole::Admin,
            identity_key: Base64Bytes::new(vec![1; 32]),
          }],
          ..RosterDocument::default()
        });
      }
      assert_eq!(history.tenures.len(), MAX_ADMIN_TENURES);
      assert!(!history.is_complete);
    }
  }

  mod owner_status {
    use super::*;

    #[test]
    fn an_owner_whose_key_is_unusable_is_listed_and_not_re_signed_silently() {
      let (alice, bob) = (Owner::new(1), Owner::new(2));
      let mut state = WorkspaceState::default();
      let mut first = roster(1, &[&alice, &bob], Some(1_000), &state);
      first.members[1].identity_key = Base64Bytes::new(vec![7; 5]);
      state
        .apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 1, &first)])
        .unwrap();
      let status = state.owner_status(100).unwrap();
      assert_eq!(
        status.owners.iter().map(|(user, _)| *user).collect::<Vec<_>>(),
        vec![alice.user_id]
      );
      assert_eq!(status.unverifiable_owners, vec![bob.user_id]);
      // Keeping Bob means signing his entry again, which can't be done as it is: refused.
      let keep = [
        (alice.user_id, RosterRole::Owner),
        (bob.user_id, RosterRole::Owner),
      ];
      assert_eq!(kept_unusable_entry(&state.roster, &keep), Some(bob.user_id));
      // Removing him explicitly is fine.
      assert_eq!(
        kept_unusable_entry(&state.roster, &[(alice.user_id, RosterRole::Owner)]),
        None
      );
    }

    #[test]
    fn signing_times_are_kept_per_owner_and_dropped_with_the_owner() {
      let (alice, bob) = (Owner::new(1), Owner::new(2));
      let mut state = WorkspaceState::default();
      assert_eq!(state.owner_status(100), None, "nothing verified yet");
      let first = roster(1, &[&alice, &bob], Some(1_000), &state);
      state
        .apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 1, &first)])
        .unwrap();
      let mut signed_policy = policy(1, 1, &state);
      signed_policy.maybe_signed_at_ms = Some(1_500);
      state
        .apply_entries(vec![bob.sign(WorkspaceLogKind::Policy, 1, &signed_policy)])
        .unwrap();

      let status = state.owner_status(10_000).unwrap();
      let mut owners = status.owners.clone();
      owners.sort();
      assert_eq!(
        owners,
        vec![(alice.user_id, Some(1_000)), (bob.user_id, Some(1_500))]
      );
      assert_eq!(
        status.maybe_expires_at_ms,
        Some(11_000),
        "the newest roster plus the window"
      );
      assert_eq!(status.recommended_minimum, 2);

      // Bob removes Alice: her time goes with her.
      let second = roster(2, &[&bob], Some(3_000), &state);
      state
        .apply_entries(vec![bob.sign(WorkspaceLogKind::Roster, 2, &second)])
        .unwrap();
      let status = state.owner_status(10_000).unwrap();
      assert_eq!(status.owners, vec![(bob.user_id, Some(3_000))]);
      assert_eq!(status.maybe_expires_at_ms, Some(13_000));
    }

    #[test]
    fn a_chained_roster_without_a_signing_time_counts_as_expired() {
      let alice = Owner::new(1);
      let mut state = WorkspaceState::default();
      let first = roster(1, &[&alice], None, &state);
      state
        .apply_entries(vec![alice.sign(WorkspaceLogKind::Roster, 1, &first)])
        .unwrap();
      let status = state.owner_status(10_000).unwrap();
      assert_eq!(status.owners, vec![(alice.user_id, None)]);
      assert_eq!(status.maybe_expires_at_ms, Some(0));
      assert!(!state.is_roster_fresh(1, 10_000), "the same as freshness says");
    }
  }

  struct Owner {
    user_id: UserId,
    identity: IdentityKeyPair,
    device: DeviceKeyPair,
    certificate: SignedDeviceCertificate,
  }

  impl Owner {
    fn new(raw: i64) -> Self {
      let user_id = UserId::from_snowflake(Snowflake::new(raw).unwrap());
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let device = DeviceKeyPair::generate(
        convert::user(user_id),
        teamcraft_crypto::ids::DeviceId::new(raw.unsigned_abs() * 100),
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

    fn sign(
      &self,
      kind: WorkspaceLogKind,
      version: u64,
      document: &impl Serialize,
    ) -> WorkspaceLogEntryResponse {
      let payload = serde_json::to_vec(document).unwrap();
      let log = if kind == WorkspaceLogKind::Roster {
        WorkspaceLog::Roster
      } else {
        WorkspaceLog::Policy
      };
      let signature = sign_log_entry(&self.device, &self.certificate, log, version, &payload).unwrap();
      WorkspaceLogEntryResponse {
        kind,
        version,
        payload: Base64Bytes::new(payload),
        signature: Base64Bytes::new(signature),
        maybe_signer_device_id: None,
        created_at: UnixMillis(0),
      }
    }
  }

  fn roster(
    version: u64,
    owners: &[&Owner],
    maybe_signed_at_ms: Option<i64>,
    state: &WorkspaceState,
  ) -> RosterDocument {
    RosterDocument {
      version,
      members: owners
        .iter()
        .map(|owner| RosterMember {
          user_id: owner.user_id,
          role: RosterRole::Owner,
          identity_key: Base64Bytes::new(owner.identity.public_key().as_bytes().to_vec()),
        })
        .collect(),
      maybe_org_recovery_key: None,
      maybe_prev_hash: Some(
        state
          .heads
          .maybe_roster_hash
          .clone()
          .unwrap_or_else(|| Base64Bytes::new(vec![0; 32])),
      ),
      maybe_signed_at_ms,
    }
  }

  fn policy(version: u64, roster_version: u64, state: &WorkspaceState) -> ChainedPolicyDocument {
    ChainedPolicyDocument {
      version,
      policy: WorkspacePolicy::default(),
      roster_version,
      maybe_prev_hash: Some(
        state
          .heads
          .maybe_policy_hash
          .clone()
          .unwrap_or_else(|| Base64Bytes::new(vec![0; 32])),
      ),
      maybe_signed_at_ms: None,
    }
  }
}
