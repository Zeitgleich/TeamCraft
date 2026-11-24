//! Compliance archive (issue #2134, e2ee.md §11, data-export.md §4.1): what member apps and the
//! vault node decide from the verified workspace policy and roster, never from the server's word.
//!
//! - Member apps: which archive receives a conversation ([`archive_of_conversation`], the banner and
//!   the device to add) and whether a device belongs to it ([`is_pinned_archive_identity`]).
//! - Admin apps: signing an export request ([`sign_export_request`]), the requester's signature and
//!   each approver's over the same bytes.
//! - The vault: whether a request is authorized ([`verify_export_request`]): enough distinct admins
//!   of the roster it verified signed it, the requester among them, archiving is on, and the request
//!   is recent. The server relays requests and signatures; it can withhold one, never forge one.

use std::collections::BTreeSet;
use std::time::Duration;

use teamcraft_crypto::identity::{DeviceKeyPair, IdentityPublicKey, SignedDeviceCertificate};
use teamcraft_crypto::workspace::{WorkspaceLog, claimed_signer, sign_log_entry, verify_log_entry};
use teamcraft_proto::{
  ArchiveAccount, Base64Bytes, ComplianceArchiveMode, ComplianceExportRequest, ComplianceExportRequestError,
  ConversationId, ConversationResponse, EXPORT_REQUEST_FORMAT, UserId,
};

use crate::convert;
use crate::custom_emoji::OneKey;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::workspace::{RosterRole, WorkspaceState};

/// How long a signed request stays good when the vault's configuration names no other age:
/// approvals take a while, a replay months later must not work.
pub const DEFAULT_EXPORT_REQUEST_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);
/// How far a requester's clock may run ahead of the vault's.
pub const EXPORT_REQUEST_CLOCK_LEEWAY: Duration = Duration::from_secs(5 * 60);

/// A request the vault may carry out, with who authorized it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedExport {
  pub request: ComplianceExportRequest,
  /// The admins whose signatures verified, the requester included, in ID order.
  pub approved_by: Vec<UserId>,
}

/// Why the vault refuses an export request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExportAuthorizationError {
  #[error("the request is not a compliance export request: {0}")]
  Malformed(String),
  #[error("the request is invalid: {0}")]
  Invalid(#[from] ComplianceExportRequestError),
  #[error("compliance archiving is not on in the verified policy")]
  ArchivingOff,
  #[error("the request is older than the vault accepts")]
  Expired,
  #[error("the request is dated in the future")]
  FromTheFuture,
  #[error("the requester's own signature is missing or doesn't verify")]
  RequesterUnsigned,
  #[error("{have} of {need} admins approved the request")]
  NotEnoughApprovals { have: usize, need: usize },
}

/// The archive that receives `conversation` under the verified policy, if archiving is on and
/// covers it and this device knows the conversation; `None` before a workspace was verified. Every
/// app reads the banner ("archived by your organization") and the archive device to add from this.
pub fn archive_of_conversation(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Option<ArchiveAccount> {
  let state: WorkspaceState = records::get_json(store, Table::Workspace, crate::client::WORKSPACE_KEY)
    .ok()
    .flatten()?;
  let known: ConversationResponse = records::get_json(store, Table::Conversations, &conversation.to_string())
    .ok()
    .flatten()?;
  state
    .policy
    .compliance_archive
    .archive_covering(&known.kind, known.maybe_space_id)
    .cloned()
}

/// Whether a device of `user` with the identity key `identity` belongs to the pinned archive: the
/// account and its key's fingerprint must both be the ones the owners signed, so the server can't
/// present another account's, or another key's, device as the archive.
pub fn is_pinned_archive_identity(
  archive: &ArchiveAccount,
  user: UserId,
  identity: &IdentityPublicKey,
) -> bool {
  archive.user_id == user
    && identity.fingerprint().as_bytes().as_slice() == archive.identity_fingerprint.as_slice()
}

/// Signs a request's bytes on an admin's device: the requester signs the bytes it sends, each
/// approver the bytes the server relays, after showing what they say.
pub fn sign_export_request(
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
  payload: &[u8],
) -> Result<Base64Bytes, CoreError> {
  let request = parse(payload).map_err(|error| CoreError::InvalidRequest(error.to_string()))?;
  request
    .check()
    .map_err(|error| CoreError::InvalidRequest(error.to_string()))?;
  let signature = sign_log_entry(
    device,
    certificate,
    WorkspaceLog::ComplianceExport,
    EXPORT_REQUEST_FORMAT,
    payload,
  )?;
  Ok(Base64Bytes::new(signature))
}

/// Whether the vault may carry out the request in `payload`, judged by `workspace` (the roster and
/// policy the vault verified itself) at `now_ms` by the vault's clock.
///
/// Only owners and admins of the current roster count, each once, and never the archive account;
/// a signature that doesn't verify or comes from anybody else is left out, not trusted. The policy's
/// `exports.approvals_required` applies, never fewer than two. An unknown archive mode refuses: the
/// vault exports only under a mode it understands.
pub fn verify_export_request(
  payload: &[u8],
  signatures: &[Base64Bytes],
  workspace: &WorkspaceState,
  now_ms: i64,
  max_age: Duration,
) -> Result<AuthorizedExport, ExportAuthorizationError> {
  let request = parse(payload)?;
  request.check()?;
  let archive_policy = &workspace.policy.compliance_archive;
  let Some(archive) = archive_policy.maybe_archive.as_ref() else {
    return Err(ExportAuthorizationError::ArchivingOff);
  };
  if archive_policy.mode != ComplianceArchiveMode::On {
    return Err(ExportAuthorizationError::ArchivingOff);
  }
  check_age(request.requested_at.0, now_ms, max_age)?;
  let approvers: BTreeSet<UserId> = signatures
    .iter()
    .filter_map(|signature| admin_signer(signature.as_slice(), payload, workspace))
    .filter(|user| *user != archive.user_id)
    .collect();
  if !approvers.contains(&request.requested_by) {
    return Err(ExportAuthorizationError::RequesterUnsigned);
  }
  let need = archive_policy.exports.effective_approvals();
  if approvers.len() < need {
    return Err(ExportAuthorizationError::NotEnoughApprovals {
      have: approvers.len(),
      need,
    });
  }
  Ok(AuthorizedExport {
    request,
    approved_by: approvers.into_iter().collect(),
  })
}

fn parse(payload: &[u8]) -> Result<ComplianceExportRequest, ExportAuthorizationError> {
  serde_json::from_slice(payload).map_err(|error| ExportAuthorizationError::Malformed(error.to_string()))
}

fn check_age(requested_at_ms: i64, now_ms: i64, max_age: Duration) -> Result<(), ExportAuthorizationError> {
  let leeway_ms = i64::try_from(EXPORT_REQUEST_CLOCK_LEEWAY.as_millis()).unwrap_or(i64::MAX);
  let max_age_ms = i64::try_from(max_age.as_millis()).unwrap_or(i64::MAX);
  if requested_at_ms > now_ms.saturating_add(leeway_ms) {
    return Err(ExportAuthorizationError::FromTheFuture);
  }
  if now_ms.saturating_sub(requested_at_ms) > max_age_ms {
    return Err(ExportAuthorizationError::Expired);
  }
  Ok(())
}

/// The owner or admin of the current roster whose device signed `payload`, if the signature verifies
/// against the identity key the roster lists for them.
fn admin_signer(signature: &[u8], payload: &[u8], workspace: &WorkspaceState) -> Option<UserId> {
  let signer = claimed_signer(signature).ok()?;
  let user = convert::user_from_crypto(signer).ok()?;
  let verifies = workspace
    .roster
    .members
    .iter()
    .filter(|member| member.user_id == user && matches!(member.role, RosterRole::Owner | RosterRole::Admin))
    .filter_map(|member| <[u8; 32]>::try_from(member.identity_key.as_slice()).ok())
    .any(|key| {
      verify_log_entry(
        signature,
        WorkspaceLog::ComplianceExport,
        EXPORT_REQUEST_FORMAT,
        payload,
        &OneKey(signer, IdentityPublicKey::from_bytes(key)),
      )
      .is_ok()
    });
  if verifies {
    Some(user)
  } else {
    tracing::debug!(%user, "an export request signature is not an admin's; left out");
    None
  }
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use teamcraft_crypto::identity::IdentityKeyPair;
  use teamcraft_crypto::ids as crypto;
  use teamcraft_proto::{
    ArchiveCoverage, ArchiveScope, ComplianceArchivePolicy, ConversationKind, EXPORT_RECIPIENT_KEY_BYTES,
    EXPORT_REQUEST_ID_BYTES, ExportPolicy, Id, Snowflake, SpaceId, UnixMillis,
  };

  use super::*;
  use crate::workspace::{RosterDocument, RosterMember};

  const NOW_MS: i64 = 1_800_000_000_000;
  const DAY_MS: i64 = 24 * 60 * 60 * 1000;

  struct Signer {
    user: UserId,
    identity: IdentityKeyPair,
    device: DeviceKeyPair,
    certificate: SignedDeviceCertificate,
  }

  mod authorization {
    use super::*;

    #[test]
    fn two_admins_authorize() {
      let (requester, approver) = (Signer::new(1), Signer::new(2));
      let workspace = workspace(&[(&requester, RosterRole::Admin), (&approver, RosterRole::Owner)]);
      let payload = payload(&request_by(&requester));
      let signatures = [requester.sign(&payload), approver.sign(&payload)];
      let export = verify(&payload, &signatures, &workspace).unwrap();
      assert_eq!(export.approved_by, vec![requester.user, approver.user]);
    }

    #[test]
    fn one_admin_alone_is_not_enough() {
      let (requester, other) = (Signer::new(1), Signer::new(2));
      let workspace = workspace(&[(&requester, RosterRole::Admin), (&other, RosterRole::Admin)]);
      let payload = payload(&request_by(&requester));
      let signature = requester.sign(&payload);
      assert_eq!(
        verify(&payload, &[signature.clone(), signature], &workspace),
        Err(ExportAuthorizationError::NotEnoughApprovals { have: 1, need: 2 }),
        "the same admin twice counts once"
      );
    }

    #[test]
    fn the_policy_can_ask_for_more() {
      let signers: Vec<Signer> = (1..=3).map(Signer::new).collect();
      let mut workspace = workspace(&signers.iter().map(|s| (s, RosterRole::Admin)).collect::<Vec<_>>());
      workspace.policy.compliance_archive.exports = ExportPolicy {
        approvals_required: 3,
        notify_people: true,
      };
      let payload = payload(&request_by(&signers[0]));
      let two: Vec<Base64Bytes> = signers[..2].iter().map(|s| s.sign(&payload)).collect();
      assert_eq!(
        verify(&payload, &two, &workspace),
        Err(ExportAuthorizationError::NotEnoughApprovals { have: 2, need: 3 })
      );
      let three: Vec<Base64Bytes> = signers.iter().map(|s| s.sign(&payload)).collect();
      assert!(verify(&payload, &three, &workspace).is_ok());
    }

    #[test]
    fn a_policy_of_one_still_needs_two() {
      let (requester, other) = (Signer::new(1), Signer::new(2));
      let mut workspace = workspace(&[(&requester, RosterRole::Admin), (&other, RosterRole::Admin)]);
      workspace.policy.compliance_archive.exports.approvals_required = 1;
      let payload = payload(&request_by(&requester));
      assert_eq!(
        verify(&payload, &[requester.sign(&payload)], &workspace),
        Err(ExportAuthorizationError::NotEnoughApprovals { have: 1, need: 2 })
      );
    }

    #[test]
    fn the_requester_must_sign() {
      let (requester, first, second) = (Signer::new(1), Signer::new(2), Signer::new(3));
      let workspace = workspace(&[
        (&requester, RosterRole::Admin),
        (&first, RosterRole::Admin),
        (&second, RosterRole::Admin),
      ]);
      let payload = payload(&request_by(&requester));
      assert_eq!(
        verify(
          &payload,
          &[first.sign(&payload), second.sign(&payload)],
          &workspace
        ),
        Err(ExportAuthorizationError::RequesterUnsigned)
      );
    }
  }

  mod signers {
    use super::*;

    #[test]
    fn members_and_former_admins_dont_count() {
      let (requester, member, stranger) = (Signer::new(1), Signer::new(2), Signer::new(3));
      let workspace = workspace(&[(&requester, RosterRole::Admin), (&member, RosterRole::Unknown)]);
      let payload = payload(&request_by(&requester));
      let signatures = [
        requester.sign(&payload),
        member.sign(&payload),
        stranger.sign(&payload),
      ];
      assert_eq!(
        verify(&payload, &signatures, &workspace),
        Err(ExportAuthorizationError::NotEnoughApprovals { have: 1, need: 2 })
      );
    }

    #[test]
    fn the_archive_account_never_approves() {
      let (requester, vault) = (Signer::new(1), Signer::new(9));
      let mut workspace = workspace(&[(&requester, RosterRole::Admin), (&vault, RosterRole::Admin)]);
      workspace.policy.compliance_archive.maybe_archive = Some(vault.archive());
      let payload = payload(&request_by(&requester));
      assert_eq!(
        verify(
          &payload,
          &[requester.sign(&payload), vault.sign(&payload)],
          &workspace
        ),
        Err(ExportAuthorizationError::NotEnoughApprovals { have: 1, need: 2 })
      );
    }

    #[test]
    fn a_key_the_roster_doesnt_list_doesnt_count() {
      let (requester, approver) = (Signer::new(1), Signer::new(2));
      let impostor = Signer::new(2);
      let workspace = workspace(&[(&requester, RosterRole::Admin), (&approver, RosterRole::Admin)]);
      let payload = payload(&request_by(&requester));
      assert_eq!(
        verify(
          &payload,
          &[requester.sign(&payload), impostor.sign(&payload)],
          &workspace
        ),
        Err(ExportAuthorizationError::NotEnoughApprovals { have: 1, need: 2 })
      );
    }

    #[test]
    fn approvals_of_other_bytes_dont_count() {
      let (requester, approver) = (Signer::new(1), Signer::new(2));
      let workspace = workspace(&[(&requester, RosterRole::Admin), (&approver, RosterRole::Admin)]);
      let request = request_by(&requester);
      let payload = payload(&request);
      let mut wider = request.clone();
      wider.people.push(user(5));
      let other = self::payload(&wider);
      assert_eq!(
        verify(
          &payload,
          &[requester.sign(&payload), approver.sign(&other)],
          &workspace
        ),
        Err(ExportAuthorizationError::NotEnoughApprovals { have: 1, need: 2 })
      );
    }

    #[test]
    fn a_policy_signature_is_no_approval() {
      let (requester, approver) = (Signer::new(1), Signer::new(2));
      let workspace = workspace(&[(&requester, RosterRole::Admin), (&approver, RosterRole::Admin)]);
      let payload = payload(&request_by(&requester));
      let policy_kind = sign_log_entry(
        &approver.device,
        &approver.certificate,
        WorkspaceLog::Policy,
        EXPORT_REQUEST_FORMAT,
        &payload,
      )
      .unwrap();
      assert_eq!(
        verify(
          &payload,
          &[requester.sign(&payload), Base64Bytes::new(policy_kind)],
          &workspace
        ),
        Err(ExportAuthorizationError::NotEnoughApprovals { have: 1, need: 2 })
      );
    }

    #[test]
    fn garbage_signatures_are_left_out() {
      let (requester, approver) = (Signer::new(1), Signer::new(2));
      let workspace = workspace(&[(&requester, RosterRole::Admin), (&approver, RosterRole::Admin)]);
      let payload = payload(&request_by(&requester));
      let signatures = [
        Base64Bytes::new(Vec::new()),
        Base64Bytes::new(vec![0xff; 200]),
        requester.sign(&payload),
        approver.sign(&payload),
      ];
      assert!(verify(&payload, &signatures, &workspace).is_ok());
    }
  }

  mod refusals {
    use super::*;

    #[test]
    fn archiving_must_be_on() {
      let (requester, approver) = (Signer::new(1), Signer::new(2));
      let mut workspace = workspace(&[(&requester, RosterRole::Admin), (&approver, RosterRole::Admin)]);
      let payload = payload(&request_by(&requester));
      let signatures = [requester.sign(&payload), approver.sign(&payload)];
      for mode in [
        ComplianceArchiveMode::Off,
        ComplianceArchiveMode::Unknown("later".into()),
      ] {
        workspace.policy.compliance_archive.mode = mode;
        assert_eq!(
          verify(&payload, &signatures, &workspace),
          Err(ExportAuthorizationError::ArchivingOff)
        );
      }
      workspace.policy.compliance_archive.mode = ComplianceArchiveMode::On;
      workspace.policy.compliance_archive.maybe_archive = None;
      assert_eq!(
        verify(&payload, &signatures, &workspace),
        Err(ExportAuthorizationError::ArchivingOff)
      );
    }

    #[test]
    fn old_and_future_requests_are_refused() {
      let (requester, approver) = (Signer::new(1), Signer::new(2));
      let workspace = workspace(&[(&requester, RosterRole::Admin), (&approver, RosterRole::Admin)]);
      for (requested_at, expected) in [
        (NOW_MS - 15 * DAY_MS, Err(ExportAuthorizationError::Expired)),
        (NOW_MS + DAY_MS, Err(ExportAuthorizationError::FromTheFuture)),
        (NOW_MS + 60_000, Ok(())),
        (NOW_MS - 13 * DAY_MS, Ok(())),
      ] {
        let mut request = request_by(&requester);
        request.requested_at = UnixMillis(requested_at);
        let payload = payload(&request);
        let signatures = [requester.sign(&payload), approver.sign(&payload)];
        assert_eq!(
          verify(&payload, &signatures, &workspace).map(|_| ()),
          expected,
          "requested at {requested_at}"
        );
      }
    }

    #[test]
    fn malformed_and_invalid_requests_are_refused() {
      let requester = Signer::new(1);
      let workspace = workspace(&[(&requester, RosterRole::Admin)]);
      assert!(matches!(
        verify(b"not json", &[], &workspace),
        Err(ExportAuthorizationError::Malformed(_))
      ));
      let mut request = request_by(&requester);
      request.conversations.clear();
      assert_eq!(
        verify(&payload(&request), &[], &workspace),
        Err(ExportAuthorizationError::Invalid(
          ComplianceExportRequestError::NoSubjects
        ))
      );
    }

    #[test]
    fn an_invalid_request_is_not_signed() {
      let requester = Signer::new(1);
      let mut request = request_by(&requester);
      request.reference = String::new();
      assert!(sign_export_request(&requester.device, &requester.certificate, &payload(&request)).is_err());
    }
  }

  mod archive_identity {
    use super::*;

    #[test]
    fn only_the_pinned_account_and_key_match() {
      let vault = Signer::new(9);
      let archive = vault.archive();
      let other_key = IdentityKeyPair::generate(&mut OsRng).public_key();
      assert!(is_pinned_archive_identity(
        &archive,
        vault.user,
        &vault.identity.public_key()
      ));
      assert!(!is_pinned_archive_identity(&archive, vault.user, &other_key));
      assert!(!is_pinned_archive_identity(
        &archive,
        user(8),
        &vault.identity.public_key()
      ));
    }

    #[test]
    fn covered_conversations_follow_the_policy() {
      let vault = Signer::new(9);
      let mut policy = archive_policy(&vault);
      let space = SpaceId::from_snowflake(Snowflake::new(4).unwrap());
      assert!(policy.archive_covering(&ConversationKind::Dm, None).is_some());
      policy.covers = ArchiveCoverage {
        scope: ArchiveScope::Spaces,
        spaces: vec![space],
      };
      assert!(
        policy
          .archive_covering(&ConversationKind::Channel, Some(space))
          .is_some()
      );
      assert!(policy.archive_covering(&ConversationKind::Dm, None).is_none());
      policy.mode = ComplianceArchiveMode::Off;
      assert!(
        policy
          .archive_covering(&ConversationKind::Channel, Some(space))
          .is_none()
      );
      policy.mode = ComplianceArchiveMode::Unknown("later".into());
      assert!(
        policy
          .archive_covering(&ConversationKind::Channel, Some(space))
          .is_some(),
        "a newer mode still shows the banner"
      );
    }
  }

  impl Signer {
    fn new(raw: i64) -> Self {
      let user = user(raw);
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let device = DeviceKeyPair::generate(convert::user(user), crypto::DeviceId::new(7), &mut OsRng);
      let certificate = identity
        .certify_device(convert::user(user), &device.public_keys())
        .unwrap();
      Self {
        user,
        identity,
        device,
        certificate,
      }
    }

    fn sign(&self, payload: &[u8]) -> Base64Bytes {
      sign_export_request(&self.device, &self.certificate, payload).unwrap()
    }

    fn archive(&self) -> ArchiveAccount {
      ArchiveAccount {
        user_id: self.user,
        identity_fingerprint: Base64Bytes::new(self.identity.public_key().fingerprint().as_bytes().to_vec()),
      }
    }
  }

  fn verify(
    payload: &[u8],
    signatures: &[Base64Bytes],
    workspace: &WorkspaceState,
  ) -> Result<AuthorizedExport, ExportAuthorizationError> {
    verify_export_request(
      payload,
      signatures,
      workspace,
      NOW_MS,
      DEFAULT_EXPORT_REQUEST_MAX_AGE,
    )
  }

  fn workspace(members: &[(&Signer, RosterRole)]) -> WorkspaceState {
    let mut state = WorkspaceState {
      roster: RosterDocument {
        members: members
          .iter()
          .map(|(signer, role)| RosterMember {
            user_id: signer.user,
            identity_key: Base64Bytes::new(signer.identity.public_key().as_bytes().to_vec()),
            role: role.clone(),
          })
          .collect(),
        ..RosterDocument::default()
      },
      ..WorkspaceState::default()
    };
    state.policy.compliance_archive = archive_policy(&Signer::new(99));
    state
  }

  fn archive_policy(vault: &Signer) -> ComplianceArchivePolicy {
    ComplianceArchivePolicy {
      mode: ComplianceArchiveMode::On,
      maybe_archive: Some(vault.archive()),
      ..WorkspaceState::default().policy.compliance_archive
    }
  }

  fn request_by(requester: &Signer) -> ComplianceExportRequest {
    ComplianceExportRequest {
      request_id: Base64Bytes::new(vec![1; EXPORT_REQUEST_ID_BYTES]),
      requested_by: requester.user,
      requested_at: UnixMillis(NOW_MS - 1000),
      policy_version: 1,
      conversations: vec![ConversationId::from_snowflake(Snowflake::new(5).unwrap())],
      people: Vec::new(),
      maybe_from: None,
      maybe_until: None,
      reference: "Case 1".into(),
      recipient_key: Base64Bytes::new(vec![2; EXPORT_RECIPIENT_KEY_BYTES]),
    }
  }

  fn payload(request: &ComplianceExportRequest) -> Vec<u8> {
    serde_json::to_vec(request).unwrap()
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
