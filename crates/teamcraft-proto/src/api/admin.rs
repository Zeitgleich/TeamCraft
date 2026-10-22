//! Administration (admin.md §3): metadata of all spaces, channels, users and devices, and the audit
//! log. Admins see no message content: DMs are never listed, and channel contents stay encrypted.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{
  AuditEntryId, ConversationId, ConversationVisibility, DeviceId, JoinPolicy, PushProvider, SpaceId,
  UnixMillis, UserId, UserKind,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminSpaceSummary {
  pub id: SpaceId,
  pub name: String,
  pub join_policy: JoinPolicy,
  pub is_archived: bool,
  pub member_count: u32,
  pub guest_count: u32,
  pub channel_count: u32,
  pub created_at: UnixMillis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListAdminSpacesResponse {
  pub spaces: Vec<AdminSpaceSummary>,
}

/// Query of `GET /api/v1/admin/conversations`. Only channels are listed, public and private.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListAdminConversationsQuery {
  #[serde(rename = "space_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_space_id: Option<SpaceId>,
  #[serde(rename = "visibility", default, skip_serializing_if = "Option::is_none")]
  pub maybe_visibility: Option<ConversationVisibility>,
  /// `true`: only channels with guests; `false`: only channels without.
  #[serde(rename = "has_guests", default, skip_serializing_if = "Option::is_none")]
  pub maybe_has_guests: Option<bool>,
  /// Only channels without a message for this many days.
  #[serde(rename = "inactive_days", default, skip_serializing_if = "Option::is_none")]
  pub maybe_inactive_days: Option<u32>,
  #[serde(rename = "min_members", default, skip_serializing_if = "Option::is_none")]
  pub maybe_min_members: Option<u32>,
  #[serde(rename = "max_members", default, skip_serializing_if = "Option::is_none")]
  pub maybe_max_members: Option<u32>,
  /// `true`: only channels without an active channel admin (they need a new owner).
  #[serde(rename = "orphaned", default, skip_serializing_if = "Option::is_none")]
  pub maybe_is_orphaned: Option<bool>,
  /// Only channels this user is a member of; the admin client needs them to sign the removals when
  /// it deactivates the user (security-review A8).
  #[serde(rename = "user_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_user_id: Option<UserId>,
  /// Continue after this channel (ascending IDs).
  #[serde(rename = "after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_after: Option<ConversationId>,
  #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none")]
  pub maybe_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminConversationSummary {
  pub id: ConversationId,
  #[serde(rename = "space_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_space_id: Option<SpaceId>,
  #[serde(rename = "name", default, skip_serializing_if = "Option::is_none")]
  pub maybe_name: Option<String>,
  pub visibility: ConversationVisibility,
  pub is_archived: bool,
  pub member_count: u32,
  pub guest_count: u32,
  pub message_count: u64,
  pub created_at: UnixMillis,
  /// Time of the newest message, if there is one.
  #[serde(rename = "last_activity_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_activity_at: Option<UnixMillis>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListAdminConversationsResponse {
  pub conversations: Vec<AdminConversationSummary>,
  pub has_more: bool,
}

wire_enum! {
  /// Which accounts a user search returns.
  pub enum AdminUserStatus {
    Active => "active",
    Deactivated => "deactivated",
    All => "all",
  }
}

wire_enum! {
  /// Why an account was deactivated.
  pub enum DeactivationReason {
    /// By a server admin.
    Admin => "admin",
    /// The identity provider disabled the user or removed their TeamCraft roles.
    Oidc => "oidc",
  }
}

/// Query of `GET /api/v1/admin/users`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListAdminUsersQuery {
  #[serde(default)]
  pub query: String,
  #[serde(rename = "status", default, skip_serializing_if = "Option::is_none")]
  pub maybe_status: Option<AdminUserStatus>,
  #[serde(rename = "after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_after: Option<UserId>,
  #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none")]
  pub maybe_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminUserSummary {
  pub id: UserId,
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  pub maybe_username: Option<String>,
  pub display_name: String,
  pub kind: UserKind,
  /// Role according to the last token the identity provider issued.
  #[serde(rename = "oidc_role", default, skip_serializing_if = "Option::is_none")]
  pub maybe_oidc_role: Option<String>,
  pub is_deactivated: bool,
  #[serde(
    rename = "deactivation_reason",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_deactivation_reason: Option<DeactivationReason>,
  pub device_count: u32,
  pub space_count: u32,
  #[serde(rename = "last_seen_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_seen_at: Option<UnixMillis>,
  pub created_at: UnixMillis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListAdminUsersResponse {
  pub users: Vec<AdminUserSummary>,
  pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminDeviceResponse {
  pub id: DeviceId,
  pub name: String,
  pub is_revoked: bool,
  #[serde(rename = "push_provider", default, skip_serializing_if = "Option::is_none")]
  pub maybe_push_provider: Option<PushProvider>,
  #[serde(rename = "last_seen_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_seen_at: Option<UnixMillis>,
  pub created_at: UnixMillis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListAdminDevicesResponse {
  pub devices: Vec<AdminDeviceResponse>,
}

/// `POST /api/v1/admin/users/{id}/deactivate`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeactivateUserRequest {
  /// Free text for the audit log (no personal data please).
  #[serde(rename = "note", default, skip_serializing_if = "Option::is_none")]
  pub maybe_note: Option<String>,
}

/// Query of `GET /api/v1/admin/audit` and `GET /api/v1/me/security-log`; newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListAuditQuery {
  #[serde(rename = "actor", default, skip_serializing_if = "Option::is_none")]
  pub maybe_actor: Option<UserId>,
  #[serde(rename = "action", default, skip_serializing_if = "Option::is_none")]
  pub maybe_action: Option<String>,
  #[serde(rename = "target_kind", default, skip_serializing_if = "Option::is_none")]
  pub maybe_target_kind: Option<String>,
  #[serde(rename = "target_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_target_id: Option<String>,
  #[serde(rename = "since", default, skip_serializing_if = "Option::is_none")]
  pub maybe_since: Option<UnixMillis>,
  #[serde(rename = "until", default, skip_serializing_if = "Option::is_none")]
  pub maybe_until: Option<UnixMillis>,
  /// Continue with entries older than this one.
  #[serde(rename = "before", default, skip_serializing_if = "Option::is_none")]
  pub maybe_before: Option<AuditEntryId>,
  #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none")]
  pub maybe_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditEntryResponse {
  pub id: AuditEntryId,
  #[serde(rename = "actor_user_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_actor_user_id: Option<UserId>,
  pub action: String,
  #[serde(rename = "target_kind", default, skip_serializing_if = "Option::is_none")]
  pub maybe_target_kind: Option<String>,
  #[serde(rename = "target_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_target_id: Option<String>,
  /// JSON object as text. Metadata only, never message content.
  pub detail: String,
  pub created_at: UnixMillis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListAuditResponse {
  pub entries: Vec<AuditEntryResponse>,
  pub has_more: bool,
}

wire_enum! {
  /// How far a push provider is usable.
  pub enum PushProviderState {
    /// The server sends pushes through it.
    Active => "active",
    /// The server has no credentials for it.
    NotConfigured => "not_configured",
    /// The sender is a stub; nothing is delivered yet.
    NotImplemented => "not_implemented",
  }
}

/// One push provider and what the server can do with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PushProviderStatus {
  pub provider: PushProvider,
  pub state: PushProviderState,
  /// Live devices registered with this provider.
  pub device_count: u32,
}

/// The media server (SFU) as the server sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SfuStatus {
  /// `[calls]` is configured: clients can get join tokens.
  pub is_configured: bool,
  #[serde(rename = "url", default, skip_serializing_if = "Option::is_none")]
  pub maybe_url: Option<String>,
  /// A registration token is set, so the SFU can report participants.
  pub is_reporting_enabled: bool,
  /// The server can forward host controls (mute, remove, end) to the SFU.
  pub has_host_controls: bool,
  /// When the SFU last contacted the server.
  #[serde(rename = "last_contact_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_contact_at: Option<UnixMillis>,
  pub active_calls: u32,
}

/// Storage the server knows about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StorageStatus {
  /// `[storage]` is configured.
  pub is_configured: bool,
  pub attachment_count: u64,
  pub attachment_bytes: u64,
  pub index_segment_bytes: u64,
}

/// `GET /api/v1/admin/instance`: what the instance runs on. No secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminInstanceResponse {
  pub sfu: SfuStatus,
  pub push: Vec<PushProviderStatus>,
  pub storage: StorageStatus,
  /// Telephony (SIP, hotline) is not part of this server yet.
  pub is_telephony_available: bool,
}
