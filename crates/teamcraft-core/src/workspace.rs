//! The verified workspace state: admin roster and policy (admin.md §2.3, e2ee.md §11).
//!
//! The server stores both logs but makes no trust decision. The client verifies them as a chain:
//! the first roster entry names the first owner and is signed by them – that identity becomes the
//! workspace root of trust, pinned on first use. Every later roster or policy entry must be signed
//! by an owner of the roster verified up to that point. An entry that fails stops the chain; the
//! client keeps the last verified state and reports the rejection.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::ids as crypto;
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_crypto::workspace::{WorkspaceLog, verify_log_entry};
use teamcraft_proto::{
  Base64Bytes, PolicyDocument, UserId, WorkspaceLogEntryResponse, WorkspaceLogKind, WorkspacePolicy,
};

use crate::convert;

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

  /// Verifies and applies new log entries of both logs, in the order they were written. Stops at
  /// the first invalid entry and records why.
  pub fn apply_entries(&mut self, mut entries: Vec<WorkspaceLogEntryResponse>) -> Result<(), WorkspaceError> {
    // Roster first on equal timestamps: a new owner's first policy may follow in the same second.
    entries.sort_by_key(|entry| {
      (
        entry.created_at.0,
        entry.kind != WorkspaceLogKind::Roster,
        entry.version,
      )
    });
    for entry in &entries {
      let result = match entry.kind {
        WorkspaceLogKind::Roster => self.apply_roster(entry),
        WorkspaceLogKind::Policy => self.apply_policy(entry),
        WorkspaceLogKind::Unknown(_) => Ok(()),
      };
      if let Err(error) = result {
        self.maybe_rejection = Some(error.to_string());
        return Err(error);
      }
    }
    Ok(())
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
    self.roster = document;
    Ok(())
  }

  fn apply_policy(&mut self, entry: &WorkspaceLogEntryResponse) -> Result<(), WorkspaceError> {
    let log = "policy";
    if entry.version <= self.policy_version {
      return Ok(());
    }
    expect_next(log, entry.version, self.policy_version)?;
    let document: PolicyDocument = parse(log, entry)?;
    if document.version != entry.version {
      return Err(payload_error(
        log,
        entry,
        "document version differs from the entry",
      ));
    }
    verify(log, WorkspaceLog::Policy, entry, &owner_directory(&self.roster))?;
    self.policy_version = entry.version;
    self.policy = document.policy;
    Ok(())
  }
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
