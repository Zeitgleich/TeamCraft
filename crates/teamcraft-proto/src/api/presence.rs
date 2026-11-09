use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{UnixMillis, UserId};

wire_enum! {
  /// What other users see (ARCHITECTURE.md §5.3).
  ///
  /// `Busy` and `BeRightBack` (issue #1578) are newer than the other states: on the wire they go
  /// as their [`legacy`](Self::legacy) state plus a `detailed_state` field (see
  /// [`state_with_detail`]), so clients that don't know them show the user online or away.
  pub enum PresenceState {
    Online => "online",
    /// Set by hand, and by the server while the user is in a call or a meeting.
    Busy => "busy",
    /// A short absence, set by hand.
    BeRightBack => "brb",
    Away => "away",
    DoNotDisturb => "dnd",
    Offline => "offline",
  }
}

impl PresenceState {
  /// What clients that know only online, away, do-not-disturb and offline show instead.
  pub fn legacy(&self) -> Self {
    match self {
      Self::Busy => Self::Online,
      Self::BeRightBack => Self::Away,
      other => other.clone(),
    }
  }
}

/// Serde helpers for a [`PresenceState`] field that older clients must still understand: used with
/// `#[serde(flatten, with = "…")]`, the field goes as its legacy state under `state` and, when that
/// differs, as itself under `detailed_state`. Reading takes the detailed state when it is one this
/// version knows, else the legacy one.
macro_rules! state_with_detail {
  ($module:ident, $key:literal, $detail:literal) => {
    #[doc = concat!("A [`PresenceState`] as `", $key, "` plus `", $detail, "` (see [`PresenceState::legacy`]).")]
    pub mod $module {
      use serde::{Deserialize, Deserializer, Serialize, Serializer};

      use super::PresenceState;

      #[derive(Serialize)]
      struct Written<'a> {
        #[serde(rename = $key)]
        legacy: PresenceState,
        #[serde(rename = $detail, skip_serializing_if = "Option::is_none")]
        maybe_detail: Option<&'a PresenceState>,
      }

      #[derive(Deserialize)]
      struct Read {
        #[serde(rename = $key)]
        legacy: PresenceState,
        #[serde(rename = $detail, default)]
        maybe_detail: Option<PresenceState>,
      }

      pub fn serialize<S: Serializer>(state: &PresenceState, serializer: S) -> Result<S::Ok, S::Error> {
        let legacy = state.legacy();
        let maybe_detail = (legacy != *state).then_some(state);
        Written { legacy, maybe_detail }.serialize(serializer)
      }

      pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PresenceState, D::Error> {
        let Read { legacy, maybe_detail } = Read::deserialize(deserializer)?;
        Ok(match maybe_detail {
          // A state newer than this version: the legacy one says at least online or away.
          Some(PresenceState::Unknown(_)) | None => legacy,
          Some(detail) => detail,
        })
      }
    }
  };
}

state_with_detail!(state_with_detail, "state", "detailed_state");
state_with_detail!(presence_with_detail, "presence", "detailed_presence");

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
    /// No override: online/away follow device activity, busy follows calls and meetings.
    Auto => "auto",
    /// "Busy" chosen by hand (issue #1578). Notifications still come; only do-not-disturb holds
    /// them back.
    Busy => "busy",
    /// "Be right back": a short absence.
    BeRightBack => "brb",
    /// "Away" chosen by hand: shown while a device is connected, whatever the devices report,
    /// until the user picks another status.
    Away => "away",
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
  #[serde(flatten, with = "state_with_detail")]
  #[schema(inline)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetCustomStatusRequest {
  #[serde(rename = "emoji", default, skip_serializing_if = "Option::is_none")]
  pub maybe_emoji: Option<String>,
  #[serde(rename = "text", default, skip_serializing_if = "Option::is_none")]
  pub maybe_text: Option<String>,
  /// After this time (the server's clock) the status disappears by itself.
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
  /// The status disappears this long after the server took the request. Wins over `expires_at`:
  /// a device whose clock is off still gets "for 30 minutes", where an end computed on its own
  /// clock could look past to the server. Older servers ignore it and use `expires_at`.
  #[serde(
    rename = "expires_in_seconds",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_expires_in_seconds: Option<u64>,
}

impl SetCustomStatusRequest {
  /// The request for `status` sent at `now`, both on the sender's clock. The end goes as a
  /// duration the server resolves on its own clock (the end itself goes along for servers that
  /// don't know durations yet). `None` if the status already ended: nothing is left to set.
  pub fn for_status(status: &CustomStatus, now: UnixMillis) -> Option<Self> {
    let maybe_expires_in_seconds = match status.maybe_expires_at {
      Some(at) if at.0 <= now.0 => return None,
      // Rounded up: the status never ends before the chosen time.
      Some(at) => Some(u64::try_from((at.0 - now.0 + 999) / 1000).unwrap_or(u64::MAX)),
      None => None,
    };
    Some(Self {
      maybe_emoji: status.maybe_emoji.clone(),
      maybe_text: status.maybe_text.clone(),
      maybe_expires_at: status.maybe_expires_at,
      maybe_expires_in_seconds,
    })
  }
}

/// `PUT /api/v1/me/presence`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetPresenceRequest {
  pub manual: ManualPresence,
  /// When the manual status ends and presence is automatic again. Without it the status stays
  /// until changed. Older servers accept it only for do-not-disturb.
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn manual_presence_wire_values_are_stable() {
    for (manual, wire) in [
      (ManualPresence::Auto, "auto"),
      (ManualPresence::Busy, "busy"),
      (ManualPresence::BeRightBack, "brb"),
      (ManualPresence::Away, "away"),
      (ManualPresence::DoNotDisturb, "dnd"),
      (ManualPresence::Invisible, "invisible"),
    ] {
      let request = SetPresenceRequest {
        manual,
        maybe_until: None,
      };
      let json = format!(r#"{{"manual":"{wire}"}}"#);
      assert_eq!(serde_json::to_string(&request).unwrap(), json);
      assert_eq!(
        serde_json::from_str::<SetPresenceRequest>(&json).unwrap(),
        request
      );
    }
  }

  #[test]
  fn an_unknown_manual_presence_is_kept() {
    let request: SetPresenceRequest = serde_json::from_str(r#"{"manual":"lunch"}"#).unwrap();
    assert_eq!(request.manual, ManualPresence::Unknown("lunch".into()));
    assert_eq!(serde_json::to_string(&request).unwrap(), r#"{"manual":"lunch"}"#);
  }

  mod detailed_state {
    use super::*;

    #[test]
    fn states_older_clients_know_go_alone() {
      for (state, wire) in [
        (PresenceState::Online, "online"),
        (PresenceState::Away, "away"),
        (PresenceState::DoNotDisturb, "dnd"),
        (PresenceState::Offline, "offline"),
      ] {
        let json = format!(r#"{{"user_id":"7","state":"{wire}"}}"#);
        assert_eq!(serde_json::to_string(&presence(state.clone())).unwrap(), json);
        assert_eq!(
          serde_json::from_str::<UserPresence>(&json).unwrap(),
          presence(state)
        );
      }
    }

    #[test]
    fn newer_states_go_with_their_legacy_state() {
      for (state, json) in [
        (
          PresenceState::Busy,
          r#"{"user_id":"7","state":"online","detailed_state":"busy"}"#,
        ),
        (
          PresenceState::BeRightBack,
          r#"{"user_id":"7","state":"away","detailed_state":"brb"}"#,
        ),
      ] {
        assert_eq!(serde_json::to_string(&presence(state.clone())).unwrap(), json);
        assert_eq!(
          serde_json::from_str::<UserPresence>(json).unwrap(),
          presence(state)
        );
      }
    }

    #[test]
    fn an_older_client_reads_the_legacy_state() {
      #[derive(Deserialize)]
      struct OlderPresence {
        state: PresenceState,
      }
      let json = serde_json::to_string(&presence(PresenceState::Busy)).unwrap();
      let older: OlderPresence = serde_json::from_str(&json).unwrap();
      assert_eq!(older.state, PresenceState::Online);
    }

    #[test]
    fn an_unknown_detailed_state_falls_back_to_the_legacy_one() {
      let json = r#"{"user_id":"7","state":"away","detailed_state":"in_orbit"}"#;
      let read: UserPresence = serde_json::from_str(json).unwrap();
      assert_eq!(read.state, PresenceState::Away);
    }

    #[test]
    fn an_unknown_state_is_kept() {
      let read: UserPresence = serde_json::from_str(r#"{"user_id":"7","state":"in_orbit"}"#).unwrap();
      assert_eq!(read.state, PresenceState::Unknown("in_orbit".into()));
    }

    fn presence(state: PresenceState) -> UserPresence {
      UserPresence {
        user_id: "7".parse().unwrap(),
        state,
        maybe_status: None,
        maybe_last_seen: None,
      }
    }
  }
}
