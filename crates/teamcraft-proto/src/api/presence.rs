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

wire_enum! {
  /// What the server derived about a user without them setting a status (PLAN.md §4). Clients show
  /// it in their own language.
  pub enum PresenceActivity {
    /// An accepted meeting or a busy period in the user's calendar is going on.
    InMeeting => "in_meeting",
    /// The user is in a call.
    InCall => "in_call",
  }
}

wire_enum! {
  /// Who may see when the user was last active (PLAN.md §4).
  pub enum LastSeenVisibility {
    /// Everybody who sees the user's presence (unless the workspace switched last seen off).
    Everyone => "everyone",
    /// Nobody.
    Nobody => "nobody",
  }
}

/// A custom status (PLAN.md §4): "🌴 Vacation until Monday". Server-visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CustomStatus {
  #[serde(rename = "emoji", default, skip_serializing_if = "Option::is_none")]
  pub maybe_emoji: Option<String>,
  #[serde(rename = "text", default, skip_serializing_if = "Option::is_none")]
  pub maybe_text: Option<String>,
  /// After this time the status disappears by itself.
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
  /// Set instead of emoji and text when the server derived the status: a meeting or a call. For
  /// meetings `expires_at` is the end of the meeting or busy period.
  #[serde(rename = "activity", default, skip_serializing_if = "Option::is_none")]
  pub maybe_activity: Option<PresenceActivity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UserPresence {
  pub user_id: UserId,
  pub state: PresenceState,
  #[serde(rename = "status", default, skip_serializing_if = "Option::is_none")]
  pub maybe_status: Option<CustomStatus>,
  /// When the user was last active. Only given while they are offline, never for somebody who
  /// appears offline or chose to hide it, and not at all if the workspace switched last seen off.
  #[serde(rename = "last_seen", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_seen: Option<UnixMillis>,
}

/// `PUT /api/v1/me/status`: sets the caller's custom status (needs an emoji or a text). Replaces the
/// previous one; `DELETE /api/v1/me/status` clears it.
pub type SetCustomStatusRequest = CustomStatus;

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

/// `GET /api/v1/me/privacy`: the caller's privacy choices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PrivacyResponse {
  pub last_seen: LastSeenVisibility,
  /// The workspace allows last seen at all; if not, the choice has no effect.
  pub is_last_seen_available: bool,
}

/// `PUT /api/v1/me/privacy`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdatePrivacyRequest {
  pub last_seen: LastSeenVisibility,
}
