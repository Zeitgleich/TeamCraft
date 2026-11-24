//! Compliance archive (issue #2134, e2ee.md §11, data-export.md §4.1): the parts of the
//! `compliance_archive` group of the owner-signed workspace policy that name the archive, what it
//! covers and how exports are approved, and the export request that admins sign.
//!
//! The archive is a member like any other: a device of a dedicated archive account (the vault node)
//! that member apps add to covered conversations. The policy pins that account's identity key by its
//! fingerprint, so the server can't slip in another "archive" device; every member reads the same
//! signed policy, so archiving can't be switched on for some members and hidden from others without
//! withholding the policy itself.
//!
//! An export is a [`ComplianceExportRequest`] that admins sign with their device keys, the requester
//! first; the vault exports only once the policy's `exports.approvals_required` distinct admins of
//! the roster it verified have signed the same bytes (teamcraft-core `compliance_export`).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{Base64Bytes, ConversationId, ConversationKind, SpaceId, UnixMillis, UserId};

/// Length of the archive account's identity key fingerprint (SHA-256, teamcraft-crypto
/// `IdentityPublicKey::fingerprint`).
pub const ARCHIVE_FINGERPRINT_BYTES: usize = 32;
/// The most spaces a policy may name in `covers.spaces`.
pub const MAX_ARCHIVE_SPACES: usize = 500;
/// The fewest admins that must sign an export: the requester and one more. One admin alone can never
/// read other people's messages (admin.md §2.1).
pub const MIN_EXPORT_APPROVALS: u8 = 2;
/// The most approvals a policy may ask for.
pub const MAX_EXPORT_APPROVALS: u8 = 10;
/// Approvals asked for when the policy names none.
pub const DEFAULT_EXPORT_APPROVALS: u8 = 2;
/// The longest export reference, in characters (as a legal hold's).
pub const MAX_EXPORT_REFERENCE_CHARS: usize = crate::MAX_LEGAL_HOLD_REFERENCE_CHARS;
/// The most conversations and people one export request may name together.
pub const MAX_EXPORT_SUBJECTS: usize = 500;
/// Length of an export request's ID (random, chosen by the requester's app).
pub const EXPORT_REQUEST_ID_BYTES: usize = 16;
/// Length of the recipient's public key: X25519, for HPKE (RFC 9180) base mode.
pub const EXPORT_RECIPIENT_KEY_BYTES: usize = 32;
/// The format version of [`ComplianceExportRequest`]; the signed "version" of its signatures.
pub const EXPORT_REQUEST_FORMAT: u64 = 1;

/// The archive account the owners pinned: only its devices whose identity key has this fingerprint
/// are added to covered conversations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ArchiveAccount {
  pub user_id: UserId,
  /// SHA-256 fingerprint of the account's identity public key, as the owner compared it with the
  /// one the vault node shows when it is set up.
  pub identity_fingerprint: Base64Bytes,
}

wire_enum! {
  /// Which conversations the archive covers.
  #[derive(Default)]
  pub enum ArchiveScope {
    #[default]
    /// Every channel, direct message, group and meeting chat.
    All => "all",
    /// Only the channels of the spaces in [`ArchiveCoverage::spaces`].
    Spaces => "spaces",
  }
}

/// What the archive covers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ArchiveCoverage {
  #[serde(default)]
  pub scope: ArchiveScope,
  /// The covered spaces when `scope` is `spaces`; empty otherwise.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub spaces: Vec<SpaceId>,
}

/// How exports from the archive are approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExportPolicy {
  /// Distinct admins (the requester included) that must sign a request.
  #[serde(default = "default_approvals")]
  pub approvals_required: u8,
  /// Whether the people whose messages are exported are told (by their own apps, from the request
  /// the server relays). On by default.
  #[serde(default = "default_true")]
  pub notify_people: bool,
}

/// Why an archive policy can't be signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ComplianceArchiveError {
  #[error("compliance archive mode is not known to this version")]
  UnknownMode,
  #[error("archiving is on but names no archive account")]
  MissingArchive,
  #[error("the archive's identity fingerprint must be {ARCHIVE_FINGERPRINT_BYTES} bytes")]
  InvalidFingerprint,
  #[error("the archive's coverage is not known to this version")]
  UnknownScope,
  #[error("the archive covers spaces but names none, or more than {MAX_ARCHIVE_SPACES}")]
  InvalidSpaces,
  #[error("the archive names a space twice")]
  DuplicateSpace,
  #[error("spaces are named while the archive covers everything")]
  SpacesWithoutScope,
  #[error("exports must need between {MIN_EXPORT_APPROVALS} and {MAX_EXPORT_APPROVALS} approvals")]
  InvalidApprovals,
  #[error("the archive's retention must be at least one day")]
  InvalidRetention,
}

/// An export from the archive, as admins sign it (data-export.md §4.1). The server relays it and its
/// signatures; it learns who asked, the subjects, the period and the reference, never the content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComplianceExportRequest {
  /// Random, chosen by the requester's app: the vault exports each request once.
  pub request_id: Base64Bytes,
  pub requested_by: UserId,
  /// By the requester's clock. The vault refuses requests older than its maximum age, so a signed
  /// request can't be replayed later.
  pub requested_at: UnixMillis,
  /// The policy version the requester saw; shown to approvers, the vault applies its own.
  pub policy_version: u64,
  /// The conversations to export.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub conversations: Vec<ConversationId>,
  /// The people whose messages to export, in every covered conversation the vault holds.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub people: Vec<UserId>,
  /// Messages sent from this time on; from the beginning when absent.
  #[serde(rename = "from", default, skip_serializing_if = "Option::is_none")]
  pub maybe_from: Option<UnixMillis>,
  /// Messages sent before this time; up to the request when absent.
  #[serde(rename = "until", default, skip_serializing_if = "Option::is_none")]
  pub maybe_until: Option<UnixMillis>,
  /// The organization's case reference, as for a legal hold.
  pub reference: String,
  /// The X25519 public key the archive is encrypted to (HPKE, RFC 9180); its private key stays with
  /// the recipient (an auditor, the legal department), never with the server or the vault.
  pub recipient_key: Base64Bytes,
}

/// Why an export request can't be signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ComplianceExportRequestError {
  #[error("the request ID must be {EXPORT_REQUEST_ID_BYTES} bytes")]
  InvalidId,
  #[error("the request names no conversation and no person")]
  NoSubjects,
  #[error("the request names more than {MAX_EXPORT_SUBJECTS} conversations and people")]
  TooManySubjects,
  #[error("the request names a conversation or a person twice")]
  DuplicateSubject,
  #[error("the period ends before it starts")]
  InvalidPeriod,
  #[error("the reference is empty, too long or has control characters")]
  InvalidReference,
  #[error("the recipient key must be {EXPORT_RECIPIENT_KEY_BYTES} bytes")]
  InvalidRecipientKey,
}

impl ArchiveCoverage {
  /// Whether a conversation of `kind` in `maybe_space` is covered. An unknown scope counts as all:
  /// a newer owner app's narrower coverage is read as the wider one, so no member is left without
  /// the banner.
  pub fn covers(&self, kind: &ConversationKind, maybe_space: Option<SpaceId>) -> bool {
    match self.scope {
      ArchiveScope::Spaces => {
        *kind == ConversationKind::Channel && maybe_space.is_some_and(|space| self.spaces.contains(&space))
      },
      ArchiveScope::All | ArchiveScope::Unknown(_) => true,
    }
  }

  fn check(&self) -> Result<(), ComplianceArchiveError> {
    match self.scope {
      ArchiveScope::Unknown(_) => Err(ComplianceArchiveError::UnknownScope),
      ArchiveScope::All if !self.spaces.is_empty() => Err(ComplianceArchiveError::SpacesWithoutScope),
      ArchiveScope::All => Ok(()),
      ArchiveScope::Spaces => {
        if self.spaces.is_empty() || self.spaces.len() > MAX_ARCHIVE_SPACES {
          return Err(ComplianceArchiveError::InvalidSpaces);
        }
        let mut seen = std::collections::HashSet::with_capacity(self.spaces.len());
        if self.spaces.iter().any(|space| !seen.insert(*space)) {
          return Err(ComplianceArchiveError::DuplicateSpace);
        }
        Ok(())
      },
    }
  }
}

impl Default for ExportPolicy {
  fn default() -> Self {
    Self {
      approvals_required: DEFAULT_EXPORT_APPROVALS,
      notify_people: true,
    }
  }
}

impl ExportPolicy {
  pub fn is_default(&self) -> bool {
    *self == Self::default()
  }

  /// The approvals the vault asks for: never fewer than [`MIN_EXPORT_APPROVALS`], whatever a policy
  /// says, so a policy that slipped past the checks still can't let one admin export alone.
  pub fn effective_approvals(&self) -> usize {
    usize::from(self.approvals_required.max(MIN_EXPORT_APPROVALS))
  }
}

/// Checks the archive part of the `compliance_archive` group before an owner's app signs it and
/// before the server stores it. `archive` may be named while archiving is off (pinned before the
/// switch).
pub fn check_archive_settings(
  mode: &crate::ComplianceArchiveMode,
  maybe_archive: Option<&ArchiveAccount>,
  covers: &ArchiveCoverage,
  exports: &ExportPolicy,
  maybe_retention_days: Option<u32>,
) -> Result<(), ComplianceArchiveError> {
  match mode {
    crate::ComplianceArchiveMode::Unknown(_) => return Err(ComplianceArchiveError::UnknownMode),
    crate::ComplianceArchiveMode::On if maybe_archive.is_none() => {
      return Err(ComplianceArchiveError::MissingArchive);
    },
    crate::ComplianceArchiveMode::On | crate::ComplianceArchiveMode::Off => {},
  }
  if maybe_archive.is_some_and(|archive| archive.identity_fingerprint.len() != ARCHIVE_FINGERPRINT_BYTES) {
    return Err(ComplianceArchiveError::InvalidFingerprint);
  }
  covers.check()?;
  if !(MIN_EXPORT_APPROVALS..=MAX_EXPORT_APPROVALS).contains(&exports.approvals_required) {
    return Err(ComplianceArchiveError::InvalidApprovals);
  }
  if maybe_retention_days == Some(0) {
    return Err(ComplianceArchiveError::InvalidRetention);
  }
  Ok(())
}

impl ComplianceExportRequest {
  /// Checks the request before an admin's app signs it (the requester's and each approver's) and
  /// before the vault acts on it.
  pub fn check(&self) -> Result<(), ComplianceExportRequestError> {
    if self.request_id.len() != EXPORT_REQUEST_ID_BYTES {
      return Err(ComplianceExportRequestError::InvalidId);
    }
    let subjects = self.conversations.len().saturating_add(self.people.len());
    if subjects == 0 {
      return Err(ComplianceExportRequestError::NoSubjects);
    }
    if subjects > MAX_EXPORT_SUBJECTS {
      return Err(ComplianceExportRequestError::TooManySubjects);
    }
    let mut conversations = std::collections::HashSet::with_capacity(self.conversations.len());
    let mut people = std::collections::HashSet::with_capacity(self.people.len());
    if self.conversations.iter().any(|id| !conversations.insert(*id))
      || self.people.iter().any(|id| !people.insert(*id))
    {
      return Err(ComplianceExportRequestError::DuplicateSubject);
    }
    if let (Some(from), Some(until)) = (self.maybe_from, self.maybe_until)
      && until <= from
    {
      return Err(ComplianceExportRequestError::InvalidPeriod);
    }
    if !crate::LegalHold::is_valid_reference(&self.reference) {
      return Err(ComplianceExportRequestError::InvalidReference);
    }
    if self.recipient_key.len() != EXPORT_RECIPIENT_KEY_BYTES {
      return Err(ComplianceExportRequestError::InvalidRecipientKey);
    }
    Ok(())
  }
}

fn default_approvals() -> u8 {
  DEFAULT_EXPORT_APPROVALS
}

fn default_true() -> bool {
  true
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{ComplianceArchiveMode, Id, Snowflake};

  mod settings {
    use super::*;

    #[test]
    fn off_without_archive_is_valid() {
      assert_eq!(
        check(&ComplianceArchiveMode::Off, None, &ArchiveCoverage::default()),
        Ok(())
      );
    }

    #[test]
    fn on_needs_an_archive() {
      assert_eq!(
        check(&ComplianceArchiveMode::On, None, &ArchiveCoverage::default()),
        Err(ComplianceArchiveError::MissingArchive)
      );
      assert_eq!(
        check(
          &ComplianceArchiveMode::On,
          Some(&archive()),
          &ArchiveCoverage::default()
        ),
        Ok(())
      );
    }

    #[test]
    fn unknown_mode_and_scope_are_refused() {
      assert_eq!(
        check(
          &ComplianceArchiveMode::Unknown("later".into()),
          Some(&archive()),
          &ArchiveCoverage::default()
        ),
        Err(ComplianceArchiveError::UnknownMode)
      );
      let covers = ArchiveCoverage {
        scope: ArchiveScope::Unknown("later".into()),
        spaces: Vec::new(),
      };
      assert_eq!(
        check(&ComplianceArchiveMode::On, Some(&archive()), &covers),
        Err(ComplianceArchiveError::UnknownScope)
      );
    }

    #[test]
    fn fingerprint_length_is_checked() {
      let short = ArchiveAccount {
        user_id: user(9),
        identity_fingerprint: Base64Bytes::new(vec![1; 31]),
      };
      assert_eq!(
        check(
          &ComplianceArchiveMode::Off,
          Some(&short),
          &ArchiveCoverage::default()
        ),
        Err(ComplianceArchiveError::InvalidFingerprint)
      );
    }

    #[test]
    fn spaces_are_checked() {
      let none = ArchiveCoverage {
        scope: ArchiveScope::Spaces,
        spaces: Vec::new(),
      };
      let twice = ArchiveCoverage {
        scope: ArchiveScope::Spaces,
        spaces: vec![space(1), space(1)],
      };
      let stray = ArchiveCoverage {
        scope: ArchiveScope::All,
        spaces: vec![space(1)],
      };
      let mode = ComplianceArchiveMode::On;
      assert_eq!(
        check(&mode, Some(&archive()), &none),
        Err(ComplianceArchiveError::InvalidSpaces)
      );
      assert_eq!(
        check(&mode, Some(&archive()), &twice),
        Err(ComplianceArchiveError::DuplicateSpace)
      );
      assert_eq!(
        check(&mode, Some(&archive()), &stray),
        Err(ComplianceArchiveError::SpacesWithoutScope)
      );
    }

    #[test]
    fn approvals_and_retention_are_bounded() {
      let covers = ArchiveCoverage::default();
      for approvals in [0, 1, MAX_EXPORT_APPROVALS + 1] {
        let exports = ExportPolicy {
          approvals_required: approvals,
          notify_people: true,
        };
        assert_eq!(
          check_archive_settings(&ComplianceArchiveMode::Off, None, &covers, &exports, None),
          Err(ComplianceArchiveError::InvalidApprovals),
          "{approvals} approvals"
        );
      }
      assert_eq!(
        check_archive_settings(
          &ComplianceArchiveMode::Off,
          None,
          &covers,
          &ExportPolicy::default(),
          Some(0)
        ),
        Err(ComplianceArchiveError::InvalidRetention)
      );
    }

    #[test]
    fn effective_approvals_never_below_two() {
      let lax = ExportPolicy {
        approvals_required: 1,
        notify_people: true,
      };
      assert_eq!(lax.effective_approvals(), 2);
      assert_eq!(ExportPolicy::default().effective_approvals(), 2);
    }
  }

  mod coverage {
    use super::*;

    #[test]
    fn all_covers_every_kind() {
      let covers = ArchiveCoverage::default();
      for kind in [
        ConversationKind::Channel,
        ConversationKind::Dm,
        ConversationKind::Group,
        ConversationKind::Meeting,
      ] {
        assert!(covers.covers(&kind, None));
      }
    }

    #[test]
    fn spaces_cover_only_their_channels() {
      let covers = ArchiveCoverage {
        scope: ArchiveScope::Spaces,
        spaces: vec![space(1)],
      };
      assert!(covers.covers(&ConversationKind::Channel, Some(space(1))));
      assert!(!covers.covers(&ConversationKind::Channel, Some(space(2))));
      assert!(!covers.covers(&ConversationKind::Channel, None));
      assert!(!covers.covers(&ConversationKind::Dm, Some(space(1))));
    }

    #[test]
    fn unknown_scope_reads_as_all() {
      let covers = ArchiveCoverage {
        scope: ArchiveScope::Unknown("later".into()),
        spaces: Vec::new(),
      };
      assert!(covers.covers(&ConversationKind::Dm, None));
    }
  }

  mod wire {
    use super::*;

    #[test]
    fn defaults_are_left_out_and_read_back() {
      let covers: ArchiveCoverage = serde_json::from_str("{}").unwrap();
      assert_eq!(covers, ArchiveCoverage::default());
      let exports: ExportPolicy = serde_json::from_str("{}").unwrap();
      assert_eq!(exports, ExportPolicy::default());
      assert_eq!(
        serde_json::to_string(&ArchiveCoverage {
          scope: ArchiveScope::Spaces,
          spaces: vec![space(7)],
        })
        .unwrap(),
        r#"{"scope":"spaces","spaces":["7"]}"#
      );
    }

    #[test]
    fn request_round_trips() {
      let json = serde_json::to_string(&request()).unwrap();
      assert_eq!(
        json,
        r#"{"request_id":"AQEBAQEBAQEBAQEBAQEBAQ==","requested_by":"1","requested_at":1000,"policy_version":3,"conversations":["5"],"from":10,"until":20,"reference":"Case 1","recipient_key":"AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI="}"#
      );
      let back: ComplianceExportRequest = serde_json::from_str(&json).unwrap();
      assert_eq!(back, request());
    }
  }

  mod request_checks {
    use super::*;

    #[test]
    fn valid_request_passes() {
      assert_eq!(request().check(), Ok(()));
    }

    #[test]
    fn needs_a_subject() {
      let mut empty = request();
      empty.conversations.clear();
      assert_eq!(empty.check(), Err(ComplianceExportRequestError::NoSubjects));
      empty.people.push(user(4));
      assert_eq!(empty.check(), Ok(()));
    }

    #[test]
    fn refuses_duplicates_and_too_many() {
      let mut twice = request();
      twice.people = vec![user(4), user(4)];
      assert_eq!(twice.check(), Err(ComplianceExportRequestError::DuplicateSubject));
      let mut many = request();
      many.conversations = (1..=MAX_EXPORT_SUBJECTS as i64 + 1).map(conversation).collect();
      assert_eq!(many.check(), Err(ComplianceExportRequestError::TooManySubjects));
    }

    #[test]
    fn refuses_bad_fields() {
      let mut bad_id = request();
      bad_id.request_id = Base64Bytes::new(vec![1; 8]);
      assert_eq!(bad_id.check(), Err(ComplianceExportRequestError::InvalidId));
      let mut backwards = request();
      backwards.maybe_until = Some(UnixMillis(10));
      assert_eq!(
        backwards.check(),
        Err(ComplianceExportRequestError::InvalidPeriod)
      );
      let mut no_reference = request();
      no_reference.reference = " ".into();
      assert_eq!(
        no_reference.check(),
        Err(ComplianceExportRequestError::InvalidReference)
      );
      let mut bad_key = request();
      bad_key.recipient_key = Base64Bytes::new(vec![2; 33]);
      assert_eq!(
        bad_key.check(),
        Err(ComplianceExportRequestError::InvalidRecipientKey)
      );
    }
  }

  fn check(
    mode: &ComplianceArchiveMode,
    maybe_archive: Option<&ArchiveAccount>,
    covers: &ArchiveCoverage,
  ) -> Result<(), ComplianceArchiveError> {
    check_archive_settings(mode, maybe_archive, covers, &ExportPolicy::default(), None)
  }

  fn archive() -> ArchiveAccount {
    ArchiveAccount {
      user_id: user(9),
      identity_fingerprint: Base64Bytes::new(vec![3; ARCHIVE_FINGERPRINT_BYTES]),
    }
  }

  fn request() -> ComplianceExportRequest {
    ComplianceExportRequest {
      request_id: Base64Bytes::new(vec![1; EXPORT_REQUEST_ID_BYTES]),
      requested_by: user(1),
      requested_at: UnixMillis(1000),
      policy_version: 3,
      conversations: vec![conversation(5)],
      people: Vec::new(),
      maybe_from: Some(UnixMillis(10)),
      maybe_until: Some(UnixMillis(20)),
      reference: "Case 1".into(),
      recipient_key: Base64Bytes::new(vec![2; EXPORT_RECIPIENT_KEY_BYTES]),
    }
  }

  fn user(id: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(id).unwrap())
  }

  fn space(id: i64) -> SpaceId {
    SpaceId::from_snowflake(Snowflake::new(id).unwrap())
  }

  fn conversation(id: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(id).unwrap())
  }
}
