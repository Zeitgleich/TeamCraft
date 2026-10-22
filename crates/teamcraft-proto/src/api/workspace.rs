//! The signed workspace policy and admin roster (admin.md §2.3, e2ee.md §11).
//!
//! Both are append-only logs. Owners' clients sign each entry; the server checks ordering and the
//! parts of the policy it has to enforce (retention), stores the bytes verbatim and serves them.
//! Clients verify the signatures and refuse to follow entries that are not signed by an owner.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{Base64Bytes, DeviceId, HistoryPolicy, UnixMillis};

wire_enum! {
  /// Which log an entry belongs to.
  pub enum WorkspaceLogKind {
    Roster => "roster",
    Policy => "policy",
  }
}

wire_enum! {
  /// May admins add themselves to private channels (admin.md §2.4)? Enforced by member clients.
  pub enum AdminSelfJoin {
    /// Allowed; members see an admin-badged system message.
    Visible => "visible",
    OwnerApproval => "owner_approval",
    Disabled => "disabled",
  }
}

wire_enum! {
  /// How the link preview domain list is applied by sending and receiving clients.
  pub enum LinkPreviewMode {
    Off => "off",
    AllowList => "allow_list",
    BlockList => "block_list",
  }
}

wire_enum! {
  /// Compliance archive (e2ee.md §11). Reserved: only `off` is accepted today.
  pub enum ComplianceArchiveMode {
    Off => "off",
  }
}

/// Organization recovery settings (e2ee.md §6.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgRecoveryPolicy {
  pub is_enabled_for_members: bool,
  pub is_enabled_for_guests: bool,
  /// Admins that must approve a recovery (2 by default).
  pub admins_required: u8,
}

/// How long messages are kept. `None` means unlimited.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RetentionSetting {
  #[serde(rename = "max_age_days", default, skip_serializing_if = "Option::is_none")]
  pub maybe_max_age_days: Option<u32>,
  #[serde(rename = "max_messages", default, skip_serializing_if = "Option::is_none")]
  pub maybe_max_messages: Option<u64>,
}

/// Instance-wide retention: defaults for channels without their own setting and the limits channel
/// owners must stay within.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RetentionPolicy {
  #[serde(default)]
  pub default: RetentionSetting,
  /// Owners may keep messages at most this long / this many; unlimited values are then not allowed.
  #[serde(default)]
  pub limit: RetentionSetting,
  /// Pinned messages survive retention.
  pub keep_pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LinkPreviewPolicy {
  pub mode: LinkPreviewMode,
  #[serde(default)]
  pub domains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComplianceArchivePolicy {
  pub mode: ComplianceArchiveMode,
}

/// The security and retention policy of the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkspacePolicy {
  pub admin_self_join: AdminSelfJoin,
  pub org_recovery: OrgRecoveryPolicy,
  /// Default for "share history" when adding members (guests always `from_join`).
  pub history_default: HistoryPolicy,
  pub retention: RetentionPolicy,
  pub link_previews: LinkPreviewPolicy,
  pub compliance_archive: ComplianceArchivePolicy,
}

/// The JSON document a policy entry's `payload` must contain (and the owner signs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PolicyDocument {
  /// Equals the entry's version; stops an old signed document from being replayed later.
  pub version: u64,
  pub policy: WorkspacePolicy,
}

impl Default for WorkspacePolicy {
  fn default() -> Self {
    Self {
      admin_self_join: AdminSelfJoin::Visible,
      org_recovery: OrgRecoveryPolicy {
        is_enabled_for_members: true,
        is_enabled_for_guests: false,
        admins_required: 2,
      },
      history_default: HistoryPolicy::Full,
      retention: RetentionPolicy {
        default: RetentionSetting::default(),
        limit: RetentionSetting::default(),
        keep_pinned: true,
      },
      link_previews: LinkPreviewPolicy {
        mode: LinkPreviewMode::Off,
        domains: Vec::new(),
      },
      compliance_archive: ComplianceArchivePolicy {
        mode: ComplianceArchiveMode::Off,
      },
    }
  }
}

/// `POST /api/v1/workspace/roster` and `POST /api/v1/workspace/policy`: appends an entry signed by
/// an owner's device. `expected_version` is the latest version the client knows (0 for the first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AppendWorkspaceLogRequest {
  pub expected_version: u64,
  pub payload: Base64Bytes,
  pub signature: Base64Bytes,
  pub signer_device_id: DeviceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkspaceLogEntryResponse {
  pub kind: WorkspaceLogKind,
  pub version: u64,
  pub payload: Base64Bytes,
  pub signature: Base64Bytes,
  #[serde(rename = "signer_device_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_signer_device_id: Option<DeviceId>,
  pub created_at: UnixMillis,
}

/// Query of the log endpoints: entries after a version, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListWorkspaceLogQuery {
  #[serde(rename = "after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_after: Option<u64>,
  #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none")]
  pub maybe_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListWorkspaceLogResponse {
  pub entries: Vec<WorkspaceLogEntryResponse>,
  pub has_more: bool,
}

/// `GET /api/v1/workspace/policy`: the latest entry and what the server reads out of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkspacePolicyResponse {
  /// 0 and the default policy if no entry exists yet.
  pub version: u64,
  pub policy: WorkspacePolicy,
  #[serde(rename = "entry", default, skip_serializing_if = "Option::is_none")]
  pub maybe_entry: Option<WorkspaceLogEntryResponse>,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn default_policy_round_trips_and_is_conservative() {
    let policy = WorkspacePolicy::default();
    let json = serde_json::to_string(&policy).unwrap();
    assert_eq!(serde_json::from_str::<WorkspacePolicy>(&json).unwrap(), policy);
    assert_eq!(policy.compliance_archive.mode, ComplianceArchiveMode::Off);
    assert!(json.contains(r#""admin_self_join":"visible""#));
    assert!(policy.org_recovery.is_enabled_for_members && !policy.org_recovery.is_enabled_for_guests);
  }

  #[test]
  fn unknown_policy_values_are_preserved_not_rejected() {
    let mut value: serde_json::Value = serde_json::to_value(WorkspacePolicy::default()).unwrap();
    value["admin_self_join"] = serde_json::json!("ask_twice");
    let policy: WorkspacePolicy = serde_json::from_value(value).unwrap();
    assert_eq!(policy.admin_self_join, AdminSelfJoin::Unknown("ask_twice".into()));
  }

  #[test]
  fn retention_setting_omits_unlimited_values() {
    let json = serde_json::to_string(&RetentionSetting {
      maybe_max_age_days: Some(30),
      maybe_max_messages: None,
    })
    .unwrap();
    assert_eq!(json, r#"{"max_age_days":30}"#);
    assert_eq!(
      serde_json::from_str::<RetentionSetting>("{}").unwrap(),
      RetentionSetting::default()
    );
  }
}

/// `PUT /api/v1/conversations/{id}/retention` and `PUT /api/v1/spaces/{id}/retention`: the channel's
/// or space's own setting; unset fields inherit. Must stay within the workspace limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetRetentionRequest {
  pub retention: RetentionSetting,
}

/// Retention of a channel or space and where it comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RetentionStatusResponse {
  /// The object's own setting (unset fields inherit).
  pub own: RetentionSetting,
  /// What applies: own, else the space's, else the workspace default.
  pub effective: RetentionSetting,
  /// The workspace limit the setting must stay within.
  pub limit: RetentionSetting,
  pub keep_pinned: bool,
}
