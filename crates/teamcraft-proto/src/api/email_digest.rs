//! E-mail digest of missed mentions (docs/notifications.md): the only e-mail notification, and
//! content-free by design: it says how many mentions wait, never where, from whom or what.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// `GET /api/v1/me/email-digest`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailDigestResponse {
  pub is_enabled: bool,
  /// How often the digest looks for missed mentions (the user's choice or the instance default).
  pub interval_minutes: u32,
  /// The shortest interval the instance allows.
  pub min_interval_minutes: u32,
  /// The instance sends digests and the user has an e-mail address.
  pub is_available: bool,
}

/// `PUT /api/v1/me/email-digest`: replaces the setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetEmailDigestRequest {
  pub is_enabled: bool,
  /// Absent: the instance default.
  #[serde(rename = "interval_minutes", default, skip_serializing_if = "Option::is_none")]
  pub maybe_interval_minutes: Option<u32>,
}
