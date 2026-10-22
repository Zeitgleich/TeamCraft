use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{UnixMillis, UserId};

wire_enum! {
  /// What other users see (ARCHITECTURE.md §5.3).
  pub enum PresenceState {
    Online => "online",
    Away => "away",
    DoNotDisturb => "dnd",
    Offline => "offline",
  }
}

wire_enum! {
  /// A device's own activity report.
  pub enum DeviceActivity {
    Active => "active",
    Idle => "idle",
  }
}

wire_enum! {
  /// The user's manual choice.
  pub enum ManualPresence {
    /// No override: online/away follow device activity.
    Auto => "auto",
    DoNotDisturb => "dnd",
    /// "Appear offline".
    Invisible => "invisible",
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UserPresence {
  pub user_id: UserId,
  pub state: PresenceState,
}

/// `PUT /api/v1/me/presence`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetPresenceRequest {
  pub manual: ManualPresence,
  /// For do-not-disturb: when it ends. Without it DND stays until changed.
  #[serde(rename = "until", default, skip_serializing_if = "Option::is_none")]
  pub maybe_until: Option<UnixMillis>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetPresenceResponse {
  pub manual: ManualPresence,
  #[serde(rename = "until", default, skip_serializing_if = "Option::is_none")]
  pub maybe_until: Option<UnixMillis>,
}

/// `GET /api/v1/presence?user_ids=1,2`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListPresenceResponse {
  pub presences: Vec<UserPresence>,
}

/// Query of `GET /api/v1/presence`: comma-separated user IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListPresenceQuery {
  pub user_ids: String,
}
