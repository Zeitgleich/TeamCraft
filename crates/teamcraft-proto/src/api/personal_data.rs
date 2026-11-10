//! What the server holds about the caller, for their own data export (issue #211,
//! docs/data-export.md). Messages and files are end-to-end encrypted: the server only holds their
//! ciphertext, so the caller's own client decrypts and exports them; this is the rest.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
  ConversationId, ConversationResponse, DeviceResponse, EmailDigestResponse, HolidayRegionResponse,
  NotificationSettings, PrivacyResponse, ProfileResponse, Role, ScheduledHoldReason, ScheduledMessageId,
  SpaceResponse, UnixMillis, UserId, UserKind,
};

/// The version of [`PersonalDataResponse`]'s layout; a field that changes meaning gets a new one.
pub const PERSONAL_DATA_VERSION: u32 = 1;

/// `GET /api/v1/me/personal-data`: every record of the caller the server can read, in one answer.
/// Sections the caller's role can't read are absent. The audit entries about the account are paged
/// apart (`GET /api/v1/me/security-log`), since they grow without bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PersonalDataResponse {
  /// [`PERSONAL_DATA_VERSION`] of the server that answered.
  pub version: u32,
  /// The server's clock when it read the records.
  pub generated_at: UnixMillis,
  pub account: PersonalAccount,
  #[serde(rename = "profile", default, skip_serializing_if = "Option::is_none")]
  pub maybe_profile: Option<ProfileResponse>,
  #[serde(rename = "privacy", default, skip_serializing_if = "Option::is_none")]
  pub maybe_privacy: Option<PrivacyResponse>,
  #[serde(
    rename = "notification_settings",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_notification_settings: Option<NotificationSettings>,
  #[serde(rename = "email_digest", default, skip_serializing_if = "Option::is_none")]
  pub maybe_email_digest: Option<EmailDigestResponse>,
  #[serde(rename = "holiday_region", default, skip_serializing_if = "Option::is_none")]
  pub maybe_holiday_region: Option<HolidayRegionResponse>,
  /// The caller's devices: names, push channel kind (never the token) and when they were added.
  #[serde(default)]
  pub devices: Vec<DeviceResponse>,
  /// The spaces the caller belongs to.
  #[serde(default)]
  pub spaces: Vec<SpaceResponse>,
  /// The conversations the caller is a member of, with the membership (role, read position, mutes).
  #[serde(default)]
  pub conversations: Vec<ConversationResponse>,
  /// Messages waiting to be sent later: when and where, not what (their text is sealed).
  #[serde(default)]
  pub scheduled_messages: Vec<PersonalScheduledMessage>,
  /// The encrypted key bag (e2ee.md §6): only that it exists, its version and size.
  #[serde(rename = "key_bag", default, skip_serializing_if = "Option::is_none")]
  pub maybe_key_bag: Option<PersonalKeyBag>,
}

/// The account itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PersonalAccount {
  pub id: UserId,
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  pub maybe_username: Option<String>,
  /// The name as the identity provider (or the user) set it.
  pub display_name: String,
  pub kind: UserKind,
  pub role: Role,
  /// The organization the identity provider names, shown as a badge.
  #[serde(rename = "organization", default, skip_serializing_if = "Option::is_none")]
  pub maybe_organization: Option<String>,
  pub created_at: UnixMillis,
  pub is_deactivated: bool,
}

/// A scheduled message as far as the server can read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PersonalScheduledMessage {
  pub id: ScheduledMessageId,
  pub conversation_id: ConversationId,
  pub due_at: UnixMillis,
  /// Bytes of the sealed message.
  pub size_bytes: u64,
  #[serde(rename = "hold", default, skip_serializing_if = "Option::is_none")]
  pub maybe_hold: Option<ScheduledHoldReason>,
}

/// The stored key bag, without its ciphertext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PersonalKeyBag {
  pub version: u64,
  pub size_bytes: u64,
  pub updated_at: UnixMillis,
}
