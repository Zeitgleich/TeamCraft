//! Profiles: the fields people fill in about themselves, and their avatar.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{UnixMillis, UserId, Weekday};

wire_enum! {
  /// Image types accepted for avatars.
  pub enum AvatarContentType {
    Png => "image/png",
    Jpeg => "image/jpeg",
    Webp => "image/webp",
  }
}

/// When somebody usually works, in their own time zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkingHours {
  pub weekdays: Vec<Weekday>,
  /// Minutes since local midnight, `0..1440`.
  pub start_minute: u16,
  /// Minutes since local midnight, after the start and at most 1440.
  pub end_minute: u16,
}

/// `GET /api/v1/me/profile` and `GET /api/v1/users/{id}/profile`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProfileResponse {
  pub user_id: UserId,
  #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
  pub maybe_title: Option<String>,
  #[serde(rename = "department", default, skip_serializing_if = "Option::is_none")]
  pub maybe_department: Option<String>,
  #[serde(rename = "phone", default, skip_serializing_if = "Option::is_none")]
  pub maybe_phone: Option<String>,
  /// IANA name, e.g. `Europe/Berlin`.
  #[serde(rename = "time_zone", default, skip_serializing_if = "Option::is_none")]
  pub maybe_time_zone: Option<String>,
  #[serde(rename = "working_hours", default, skip_serializing_if = "Option::is_none")]
  pub maybe_working_hours: Option<WorkingHours>,
  pub has_avatar: bool,
  /// The name the person chose instead of the one from the identity provider; absent when the
  /// provider's name is shown (issue #408).
  #[serde(
    rename = "display_name_override",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_display_name_override: Option<String>,
  /// Whether the workspace policy lets people replace the name from the identity provider
  /// (`directory.allow_display_name_override`); clients show the name field only then. Absent from
  /// older servers: no.
  #[serde(default)]
  pub can_override_display_name: bool,
  /// Changes with every new picture; clients key their cached copy by it (issue #749). Absent from
  /// older servers and when there is no picture.
  #[serde(rename = "avatar_version", default, skip_serializing_if = "Option::is_none")]
  pub maybe_avatar_version: Option<String>,
}

/// `PUT /api/v1/me/profile`: replaces the profile; fields left out are cleared.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateProfileRequest {
  /// Replaces the name from the identity provider; absent or empty falls back to it. Refused with
  /// `display_name_override_not_allowed` while the workspace policy forbids overrides.
  #[serde(rename = "display_name", default, skip_serializing_if = "Option::is_none")]
  pub maybe_display_name: Option<String>,
  #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
  pub maybe_title: Option<String>,
  #[serde(rename = "department", default, skip_serializing_if = "Option::is_none")]
  pub maybe_department: Option<String>,
  #[serde(rename = "phone", default, skip_serializing_if = "Option::is_none")]
  pub maybe_phone: Option<String>,
  #[serde(rename = "time_zone", default, skip_serializing_if = "Option::is_none")]
  pub maybe_time_zone: Option<String>,
  #[serde(rename = "working_hours", default, skip_serializing_if = "Option::is_none")]
  pub maybe_working_hours: Option<WorkingHours>,
}

impl From<&ProfileResponse> for UpdateProfileRequest {
  /// The request that saves the profile as it is, for a change to one field: the update replaces
  /// everything, so a field left out would be cleared.
  fn from(profile: &ProfileResponse) -> Self {
    Self {
      maybe_display_name: profile.maybe_display_name_override.clone(),
      maybe_title: profile.maybe_title.clone(),
      maybe_department: profile.maybe_department.clone(),
      maybe_phone: profile.maybe_phone.clone(),
      maybe_time_zone: profile.maybe_time_zone.clone(),
      maybe_working_hours: profile.maybe_working_hours.clone(),
    }
  }
}

/// `POST /api/v1/me/avatar`: announces an upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateAvatarUploadRequest {
  pub content_type: AvatarContentType,
  pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateAvatarUploadResponse {
  /// `PUT` the image here with exactly this `Content-Type` and `Content-Length`.
  pub upload_url: String,
  pub content_type: AvatarContentType,
  pub expires_at: UnixMillis,
}

/// `GET /api/v1/users/{id}/avatar`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AvatarResponse {
  pub download_url: String,
  pub content_type: AvatarContentType,
  pub size_bytes: u64,
  pub expires_at: UnixMillis,
}
