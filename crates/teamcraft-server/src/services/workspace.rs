//! The signed admin roster and workspace policy (admin.md §2.3, e2ee.md §11).
//!
//! Owners append entries signed by their devices. The server keeps the logs append-only and
//! ordered, reads out of the policy only what it must enforce (retention) and rejects what the
//! protocol reserves (compliance archive). Verifying signatures is up to the clients.

use serde_json::json;
use teamcraft_db::{AppendOutcome, WorkspaceLogEntry};
use teamcraft_proto::{
  AppendWorkspaceLogRequest, Base64Bytes, ComplianceArchiveMode, GatewayEvent, ListWorkspaceLogQuery,
  ListWorkspaceLogResponse, Permission, PolicyDocument, Target, WorkspaceLogEntryResponse, WorkspaceLogKind,
  WorkspacePolicy, WorkspacePolicyResponse,
};

use super::access::require;
use super::audit::{self, Target as AuditTarget};
use super::convert::unix_millis;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_SIGNATURE_BYTES: usize = 4096;
const MAX_LINK_PREVIEW_DOMAINS: usize = 500;
const MAX_DOMAIN_CHARS: usize = 253;
const MAX_ORG_RECOVERY_ADMINS: u8 = 10;
const MAX_STATUS_PRESETS: usize = 20;
const MAX_PRESET_TEXT_CHARS: usize = 100;
/// Organization backgrounds a policy may list.
pub const MAX_ORG_BACKGROUNDS: usize = 50;
const MAX_ORG_BACKGROUND_NAME_CHARS: usize = 100;
const SHA256_BYTES: usize = 32;

pub async fn append(
  state: &AppState,
  caller: &AuthenticatedUser,
  kind: WorkspaceLogKind,
  request: AppendWorkspaceLogRequest,
) -> Result<WorkspaceLogEntryResponse, ApiError> {
  require(state, caller, Permission::SignWorkspace, &Target::Instance)?;
  if request.payload.is_empty() || request.payload.len() > MAX_PAYLOAD_BYTES {
    return Err(ApiError::InvalidRequest("payload has an invalid size".into()));
  }
  if request.signature.is_empty() || request.signature.len() > MAX_SIGNATURE_BYTES {
    return Err(ApiError::InvalidRequest("signature has an invalid size".into()));
  }
  if !state
    .database
    .is_active_device_of(caller.user.id, request.signer_device_id)
    .await?
  {
    return Err(ApiError::device_not_found());
  }
  let maybe_document = if kind == WorkspaceLogKind::Policy {
    let document = check_policy_payload(request.payload.as_slice(), request.expected_version + 1)?;
    check_org_backgrounds_exist(state, &document.policy.call_backgrounds).await?;
    Some(document)
  } else {
    None
  };

  let outcome = state
    .database
    .append_workspace_log(
      &kind,
      request.expected_version,
      request.payload.as_slice(),
      request.signature.as_slice(),
      request.signer_device_id,
    )
    .await?;
  if let AppendOutcome::VersionConflict { current_version } = outcome {
    return Err(ApiError::VersionConflict { current_version });
  }

  let version = request.expected_version + 1;
  if let Some(document) = maybe_document {
    state.policy.set(document.policy);
  }
  audit::record(
    state,
    Some(caller.user.id),
    match kind {
      WorkspaceLogKind::Roster => "roster_appended",
      _ => "policy_appended",
    },
    None::<AuditTarget<'_>>,
    &json!({"version": version}),
  )
  .await?;
  state.hub.publish_all(&GatewayEvent::WorkspaceUpdate {
    kind: kind.clone(),
    version,
  });

  let entries = state.database.list_workspace_log(&kind, version - 1, 1).await?;
  entries
    .first()
    .map(entry_response)
    .ok_or_else(|| ApiError::Internal("appended entry missing".into()))
}

pub async fn list(
  state: &AppState,
  _caller: &AuthenticatedUser,
  kind: WorkspaceLogKind,
  query: ListWorkspaceLogQuery,
) -> Result<ListWorkspaceLogResponse, ApiError> {
  // The logs are public to every signed-in user: clients must be able to verify them.
  let limits = &state.settings.limits;
  let limit = query
    .maybe_limit
    .unwrap_or(limits.default_page_size)
    .clamp(1, limits.max_page_size);
  let mut rows = state
    .database
    .list_workspace_log(&kind, query.maybe_after.unwrap_or(0), i64::from(limit))
    .await?;
  let has_more = rows.len() > limit as usize;
  rows.truncate(limit as usize);
  Ok(ListWorkspaceLogResponse {
    entries: rows.iter().map(entry_response).collect(),
    has_more,
  })
}

/// The latest policy (the default one if none was signed yet).
pub async fn current_policy(
  state: &AppState,
) -> Result<(u64, WorkspacePolicy, Option<WorkspaceLogEntry>), ApiError> {
  match state
    .database
    .latest_workspace_log_entry(&WorkspaceLogKind::Policy)
    .await?
  {
    Some(entry) => {
      let document: PolicyDocument = serde_json::from_slice(&entry.payload)
        .map_err(|e| ApiError::Internal(format!("stored policy is unreadable: {e}")))?;
      Ok((entry.version, document.policy, Some(entry)))
    },
    None => Ok((0, WorkspacePolicy::default(), None)),
  }
}

pub async fn get_policy(
  state: &AppState,
  _caller: &AuthenticatedUser,
) -> Result<WorkspacePolicyResponse, ApiError> {
  let (version, policy, entry) = current_policy(state).await?;
  Ok(WorkspacePolicyResponse {
    version,
    policy,
    maybe_entry: entry.as_ref().map(entry_response),
  })
}

/// Parses and validates a policy document before it is appended.
fn check_policy_payload(payload: &[u8], expected_new_version: u64) -> Result<PolicyDocument, ApiError> {
  let document: PolicyDocument = serde_json::from_slice(payload)
    .map_err(|e| ApiError::InvalidRequest(format!("the payload is not a policy document: {e}")))?;
  if document.version != expected_new_version {
    return Err(ApiError::InvalidRequest(format!(
      "the document says version {}, the entry would be version {expected_new_version}",
      document.version
    )));
  }
  let policy = &document.policy;
  // Reserved: archiving can't be switched on until the vault node exists (e2ee.md §11).
  if policy.compliance_archive.mode != ComplianceArchiveMode::Off {
    return Err(ApiError::InvalidRequest(
      "compliance_archive is reserved and must be off".into(),
    ));
  }
  if policy.org_recovery.admins_required == 0 || policy.org_recovery.admins_required > MAX_ORG_RECOVERY_ADMINS
  {
    return Err(ApiError::InvalidRequest(
      "org_recovery.admins_required must be between 1 and 10".into(),
    ));
  }
  if matches!(policy.admin_self_join, teamcraft_proto::AdminSelfJoin::Unknown(_))
    || matches!(
      policy.link_previews.mode,
      teamcraft_proto::LinkPreviewMode::Unknown(_)
    )
    || matches!(policy.history_default, teamcraft_proto::HistoryPolicy::Unknown(_))
  {
    return Err(ApiError::InvalidRequest(
      "the policy contains values this server does not know".into(),
    ));
  }
  if matches!(
    policy.directory.display_name_format,
    teamcraft_proto::DisplayNameFormat::Unknown(_)
  ) {
    return Err(ApiError::InvalidRequest(
      "the policy contains values this server does not know".into(),
    ));
  }
  let presence = &policy.presence;
  if presence.maybe_away_timeout_minutes == Some(0)
    || presence.presets.len() > MAX_STATUS_PRESETS
    || presence.presets.iter().any(|preset| {
      preset.text.trim().is_empty()
        || preset.text.chars().count() > MAX_PRESET_TEXT_CHARS
        || preset
          .maybe_emoji
          .as_deref()
          .is_some_and(|emoji| emoji.chars().count() > MAX_PRESET_TEXT_CHARS)
        || preset.maybe_duration_minutes == Some(0)
    })
  {
    return Err(ApiError::InvalidRequest("invalid presence policy".into()));
  }
  if policy.session.maybe_max_age_hours == Some(0) || policy.session.maybe_idle_timeout_minutes == Some(0) {
    return Err(ApiError::InvalidRequest(
      "session limits must be positive; leave them out for no limit".into(),
    ));
  }
  if policy.link_previews.domains.len() > MAX_LINK_PREVIEW_DOMAINS
    || policy
      .link_previews
      .domains
      .iter()
      .any(|domain| !is_plausible_domain(domain))
  {
    return Err(ApiError::InvalidRequest(
      "link_previews.domains must be host names".into(),
    ));
  }
  let domains = &policy.guests.allowed_email_domains;
  if domains.len() > MAX_LINK_PREVIEW_DOMAINS || domains.iter().any(|domain| !is_plausible_domain(domain)) {
    return Err(ApiError::InvalidRequest(
      "guests.allowed_email_domains must be domain names".into(),
    ));
  }
  check_retention(&policy.retention)?;
  check_call_backgrounds(&policy.call_backgrounds)?;
  Ok(document)
}

/// The organization backgrounds' list is well-formed: known mode, unique ids, SHA-256 hashes,
/// names, a default from the list, and at least one picture when one is required.
fn check_call_backgrounds(backgrounds: &teamcraft_proto::CallBackgroundPolicy) -> Result<(), ApiError> {
  use teamcraft_proto::CallBackgroundMode;

  let invalid = |what: &str| Err(ApiError::InvalidRequest(format!("call_backgrounds: {what}")));
  if matches!(backgrounds.mode, CallBackgroundMode::Unknown(_)) {
    return Err(ApiError::InvalidRequest(
      "the policy contains values this server does not know".into(),
    ));
  }
  if backgrounds.images.len() > MAX_ORG_BACKGROUNDS {
    return invalid("too many pictures");
  }
  let mut ids = std::collections::HashSet::new();
  for image in &backgrounds.images {
    if !ids.insert(image.id) {
      return invalid("a picture is listed twice");
    }
    if image.sha256.len() != SHA256_BYTES || image.size_bytes == 0 {
      return invalid("each picture needs its SHA-256 and size");
    }
    let name = image.name.trim();
    if name.is_empty()
      || name.chars().count() > MAX_ORG_BACKGROUND_NAME_CHARS
      || name.chars().any(char::is_control)
    {
      return invalid("each picture needs a short name");
    }
  }
  if backgrounds.maybe_default.is_some_and(|id| !ids.contains(&id)) {
    return invalid("the default must be one of the pictures");
  }
  if backgrounds.mode == CallBackgroundMode::Required && backgrounds.images.is_empty() {
    return invalid("a required background needs at least one picture");
  }
  Ok(())
}

/// Every listed picture was uploaded completely and has the size the policy states (clients
/// check the hash; this only keeps owners from publishing pictures nobody can fetch).
async fn check_org_backgrounds_exist(
  state: &AppState,
  backgrounds: &teamcraft_proto::CallBackgroundPolicy,
) -> Result<(), ApiError> {
  for image in &backgrounds.images {
    let stored = state.database.get_org_background(image.id).await?;
    if !stored.is_some_and(|stored| stored.is_complete && stored.size_bytes == image.size_bytes) {
      return Err(ApiError::InvalidRequest(format!(
        "call_backgrounds: picture {} is not uploaded (or has another size)",
        image.id
      )));
    }
  }
  Ok(())
}

fn check_retention(retention: &teamcraft_proto::RetentionPolicy) -> Result<(), ApiError> {
  for setting in [&retention.default, &retention.limit] {
    if setting.maybe_max_age_days == Some(0) || setting.maybe_max_messages == Some(0) {
      return Err(ApiError::InvalidRequest(
        "retention values must be positive".into(),
      ));
    }
  }
  // A default beyond the limit would be invalid for every channel using it.
  let age_ok = match (
    retention.default.maybe_max_age_days,
    retention.limit.maybe_max_age_days,
  ) {
    (Some(default), Some(limit)) => default <= limit,
    (None, Some(_)) => false,
    _ => true,
  };
  let count_ok = match (
    retention.default.maybe_max_messages,
    retention.limit.maybe_max_messages,
  ) {
    (Some(default), Some(limit)) => default <= limit,
    (None, Some(_)) => false,
    _ => true,
  };
  if age_ok && count_ok {
    Ok(())
  } else {
    Err(ApiError::InvalidRequest(
      "the default retention must be within the limit".into(),
    ))
  }
}

fn is_plausible_domain(domain: &str) -> bool {
  !domain.is_empty()
    && domain.len() <= MAX_DOMAIN_CHARS
    && !domain.starts_with('.')
    && !domain.ends_with('.')
    && domain
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

fn entry_response(entry: &WorkspaceLogEntry) -> WorkspaceLogEntryResponse {
  WorkspaceLogEntryResponse {
    kind: entry.kind.clone(),
    version: entry.version,
    payload: Base64Bytes::new(entry.payload.clone()),
    signature: Base64Bytes::new(entry.signature.clone()),
    maybe_signer_device_id: entry.maybe_signer,
    created_at: unix_millis(entry.created_at),
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{AdminSelfJoin, RetentionSetting};

  use super::*;

  fn payload(version: u64, edit: impl FnOnce(&mut WorkspacePolicy)) -> Vec<u8> {
    let mut policy = WorkspacePolicy::default();
    edit(&mut policy);
    serde_json::to_vec(&PolicyDocument { version, policy }).unwrap()
  }

  #[test]
  fn call_background_lists_are_checked() {
    use teamcraft_proto::{Base64Bytes, CallBackgroundMode, OrgBackground};

    fn image(id: &str) -> OrgBackground {
      OrgBackground {
        id: id.parse().unwrap(),
        name: "Lobby".into(),
        sha256: Base64Bytes::new(vec![7; 32]),
        size_bytes: 1000,
      }
    }
    let valid = payload(1, |p| {
      p.call_backgrounds.mode = CallBackgroundMode::Required;
      p.call_backgrounds.images = vec![image("1"), image("2")];
      p.call_backgrounds.maybe_default = Some("2".parse().unwrap());
    });
    assert!(check_policy_payload(&valid, 1).is_ok());
    type Edit = Box<dyn FnOnce(&mut WorkspacePolicy)>;
    let failing: [Edit; 6] = [
      Box::new(|p| {
        p.call_backgrounds.mode = CallBackgroundMode::Required;
      }),
      Box::new(|p| p.call_backgrounds.images = vec![image("1"), image("1")]),
      Box::new(|p| {
        let mut short = image("1");
        short.sha256 = Base64Bytes::new(vec![1; 20]);
        p.call_backgrounds.images = vec![short];
      }),
      Box::new(|p| {
        let mut unnamed = image("1");
        unnamed.name = "  ".into();
        p.call_backgrounds.images = vec![unnamed];
      }),
      Box::new(|p| {
        p.call_backgrounds.images = vec![image("1")];
        p.call_backgrounds.maybe_default = Some("9".parse().unwrap());
      }),
      Box::new(|p| p.call_backgrounds.mode = CallBackgroundMode::Unknown("by_team".into())),
    ];
    for edit in failing {
      assert!(check_policy_payload(&payload(1, edit), 1).is_err());
    }
  }

  #[test]
  fn a_valid_document_for_the_next_version_passes() {
    assert!(check_policy_payload(&payload(3, |_| {}), 3).is_ok());
    assert!(check_policy_payload(&payload(3, |p| p.admin_self_join = AdminSelfJoin::Disabled), 3).is_ok());
  }

  #[test]
  fn replayed_or_wrongly_numbered_documents_fail() {
    assert!(check_policy_payload(&payload(2, |_| {}), 3).is_err());
    assert!(check_policy_payload(b"not json", 1).is_err());
    assert!(check_policy_payload(b"{}", 1).is_err());
  }

  #[test]
  fn the_compliance_archive_cannot_be_switched_on() {
    let mut value = serde_json::to_value(PolicyDocument {
      version: 1,
      policy: WorkspacePolicy::default(),
    })
    .unwrap();
    value["policy"]["compliance_archive"]["mode"] = json!("on");
    assert!(check_policy_payload(&serde_json::to_vec(&value).unwrap(), 1).is_err());
  }

  #[test]
  fn org_recovery_needs_a_sane_number_of_admins() {
    assert!(check_policy_payload(&payload(1, |p| p.org_recovery.admins_required = 0), 1).is_err());
    assert!(check_policy_payload(&payload(1, |p| p.org_recovery.admins_required = 11), 1).is_err());
  }

  #[test]
  fn retention_defaults_must_respect_the_limit() {
    let with = |default: RetentionSetting, limit: RetentionSetting| {
      payload(1, move |p| {
        p.retention.default = default;
        p.retention.limit = limit;
      })
    };
    let days = |n| RetentionSetting {
      maybe_max_age_days: Some(n),
      maybe_max_messages: None,
    };
    assert!(check_policy_payload(&with(days(30), days(90)), 1).is_ok());
    assert!(check_policy_payload(&with(days(120), days(90)), 1).is_err());
    assert!(
      check_policy_payload(&with(RetentionSetting::default(), days(90)), 1).is_err(),
      "unlimited exceeds a limit"
    );
    assert!(check_policy_payload(&with(days(0), RetentionSetting::default()), 1).is_err());
  }

  #[test]
  fn link_preview_domains_must_look_like_host_names() {
    let with = |domain: &str| payload(1, |p| p.link_previews.domains = vec![domain.to_owned()]);
    assert!(check_policy_payload(&with("example.com"), 1).is_ok());
    for bad in [
      "",
      "https://example.com",
      "exa mple.com",
      ".example.com",
      "example.com.",
    ] {
      assert!(check_policy_payload(&with(bad), 1).is_err(), "{bad:?}");
    }
  }
}
