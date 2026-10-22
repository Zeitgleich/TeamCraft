//! The instance-wide announcement banner (PLAN.md §13).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{AnnouncementId, UnixMillis};

wire_enum! {
  /// How loud the banner is.
  pub enum AnnouncementLevel {
    Info => "info",
    Warning => "warning",
    Critical => "critical",
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AnnouncementResponse {
  pub id: AnnouncementId,
  /// Plain text. Announcements are not end-to-end encrypted: they are for everybody.
  pub text: String,
  pub level: AnnouncementLevel,
  /// Whether people may hide it for themselves.
  pub is_dismissible: bool,
  pub created_at: UnixMillis,
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
  /// The caller dismissed it already.
  pub is_dismissed: bool,
}

/// `GET /api/v1/announcement`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GetAnnouncementResponse {
  #[serde(rename = "announcement", default, skip_serializing_if = "Option::is_none")]
  pub maybe_announcement: Option<AnnouncementResponse>,
}

/// `PUT /api/v1/admin/announcement`: replaces the current banner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetAnnouncementRequest {
  pub text: String,
  pub level: AnnouncementLevel,
  pub is_dismissible: bool,
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
}
