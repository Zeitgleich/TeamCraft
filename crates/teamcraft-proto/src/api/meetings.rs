use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{ConversationId, IdempotencyKey, MeetingId, UnixMillis, UserId, UserSummary};

wire_enum! {
  pub enum MeetingStatus {
    Scheduled => "scheduled",
    /// Cancelled by the organizer; the chat stays readable.
    Cancelled => "cancelled",
    Ended => "ended",
  }
}

wire_enum! {
  pub enum MeetingKind {
    /// Planned in advance; announced by e-mail and stored in CalDAV.
    Scheduled => "scheduled",
    /// "Meet now": starts immediately and is shared by link.
    Instant => "instant",
  }
}

wire_enum! {
  /// An invitee's answer (iCalendar `PARTSTAT`).
  pub enum RsvpStatus {
    NeedsAction => "needs_action",
    Accepted => "accepted",
    Declined => "declined",
    Tentative => "tentative",
  }
}

wire_enum! {
  pub enum RecurrenceFrequency {
    Daily => "daily",
    Weekly => "weekly",
    Monthly => "monthly",
  }
}

wire_enum! {
  /// A guest's place in the lobby of a meeting.
  pub enum LobbyState {
    Waiting => "waiting",
    Admitted => "admitted",
    Denied => "denied",
  }
}

/// A simple recurrence (iCalendar `RRULE` with `FREQ`, `INTERVAL` and `COUNT`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeetingRecurrence {
  pub frequency: RecurrenceFrequency,
  /// Every n-th day, week or month; at least 1.
  pub interval: u32,
  /// Number of occurrences; without it the series is open ended.
  #[serde(rename = "count", default, skip_serializing_if = "Option::is_none")]
  pub maybe_count: Option<u32>,
}

/// Somebody invited to a meeting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeetingInvitee {
  /// A TeamCraft user (member or guest).
  User { user_id: UserId },
  /// Somebody without an account: invited by e-mail with a personal join link.
  External {
    email: String,
    #[serde(rename = "name", default, skip_serializing_if = "Option::is_none")]
    maybe_name: Option<String>,
  },
}

/// `POST /api/v1/meetings`: schedules a meeting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateMeetingRequest {
  pub title: String,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
  pub starts_at: UnixMillis,
  pub ends_at: UnixMillis,
  #[serde(default)]
  pub invitees: Vec<MeetingInvitee>,
  #[serde(rename = "recurrence", default, skip_serializing_if = "Option::is_none")]
  pub maybe_recurrence: Option<MeetingRecurrence>,
  /// Guests wait for the host to admit them (default: yes).
  #[serde(default = "default_true")]
  pub has_lobby: bool,
  /// Client-generated key that makes a retry of this request return the original result instead
  /// of creating a second one. Optional so older clients keep working.
  #[serde(rename = "idempotency_key", default, skip_serializing_if = "Option::is_none")]
  pub maybe_idempotency_key: Option<IdempotencyKey>,
}

/// `PATCH /api/v1/meetings/{id}`: changes what is set; invitees are replaced as a whole.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateMeetingRequest {
  #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
  pub maybe_title: Option<String>,
  /// An empty string clears the description.
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
  #[serde(rename = "starts_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_starts_at: Option<UnixMillis>,
  #[serde(rename = "ends_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_ends_at: Option<UnixMillis>,
  #[serde(rename = "invitees", default, skip_serializing_if = "Option::is_none")]
  pub maybe_invitees: Option<Vec<MeetingInvitee>>,
  #[serde(rename = "has_lobby", default, skip_serializing_if = "Option::is_none")]
  pub maybe_has_lobby: Option<bool>,
}

/// `POST /api/v1/meetings/instant`: "Meet now".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateInstantMeetingRequest {
  #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
  pub maybe_title: Option<String>,
  /// Client-generated key that makes a retry of this request return the original result instead
  /// of creating a second one. Optional so older clients keep working.
  #[serde(rename = "idempotency_key", default, skip_serializing_if = "Option::is_none")]
  pub maybe_idempotency_key: Option<IdempotencyKey>,
}

/// `PUT /api/v1/meetings/{id}/rsvp`: the caller's answer to an invitation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetRsvpRequest {
  pub status: RsvpStatus,
}

/// One invitee with the answer so far.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeetingAttendeeResponse {
  #[serde(rename = "user_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_user_id: Option<UserId>,
  #[serde(rename = "email", default, skip_serializing_if = "Option::is_none")]
  pub maybe_email: Option<String>,
  #[serde(rename = "name", default, skip_serializing_if = "Option::is_none")]
  pub maybe_name: Option<String>,
  pub rsvp: RsvpStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeetingResponse {
  pub id: MeetingId,
  pub title: String,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
  pub starts_at: UnixMillis,
  pub ends_at: UnixMillis,
  pub kind: MeetingKind,
  pub status: MeetingStatus,
  pub has_lobby: bool,
  pub organizer_user_id: UserId,
  /// The meeting chat.
  pub conversation_id: ConversationId,
  /// Link for members; guests get personal links with a token (see `GuestLinkResponse`).
  pub join_url: String,
  #[serde(rename = "recurrence", default, skip_serializing_if = "Option::is_none")]
  pub maybe_recurrence: Option<MeetingRecurrence>,
  pub attendees: Vec<MeetingAttendeeResponse>,
  /// Invitees whose invitation e-mail could not be sent (only set in replies to create and update).
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub undelivered_invitations: Vec<String>,
}

/// Query of `GET /api/v1/meetings`: the caller's meetings in a time range, soonest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListMeetingsQuery {
  /// Meetings ending after this time; default: now.
  #[serde(rename = "from", default, skip_serializing_if = "Option::is_none")]
  pub maybe_from: Option<UnixMillis>,
  /// Meetings starting before this time.
  #[serde(rename = "to", default, skip_serializing_if = "Option::is_none")]
  pub maybe_to: Option<UnixMillis>,
  #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none")]
  pub maybe_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListMeetingsResponse {
  pub meetings: Vec<MeetingResponse>,
  /// The server's clock when it answered: clients schedule meeting reminders on it, not on their
  /// own clock, which may be off (robustness.md, "Time"). Absent from older servers.
  #[serde(rename = "server_now", default, skip_serializing_if = "Option::is_none")]
  pub maybe_server_now: Option<UnixMillis>,
}

/// `POST /api/v1/meetings/{id}/guest-links`: a personal join link for somebody without an account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateGuestLinkRequest {
  /// Whom the link is for, for the host's overview; not verified.
  #[serde(rename = "label", default, skip_serializing_if = "Option::is_none")]
  pub maybe_label: Option<String>,
}

/// A one-time join link. The token is in the URL fragment, which browsers never send to servers.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GuestLinkResponse {
  pub join_url: String,
  pub expires_at: UnixMillis,
}

// The link is a credential and doesn't belong in logs.
impl std::fmt::Debug for GuestLinkResponse {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("GuestLinkResponse")
      .field("join_url", &"[redacted]")
      .field("expires_at", &self.expires_at)
      .finish()
  }
}

/// `POST /api/v1/meetings/join` (no authentication): trades a personal join link for a guest session.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JoinMeetingRequest {
  /// The token from the link's `#invite=` fragment; usable once.
  pub invite_token: String,
  /// How the guest is shown to the others ("Guest (unverified)" in clients).
  pub display_name: String,
}

impl std::fmt::Debug for JoinMeetingRequest {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("JoinMeetingRequest")
      .field("invite_token", &"[redacted]")
      .field("display_name", &self.display_name)
      .finish()
  }
}

/// A guest session: authenticate API and gateway with `guest_token` like with an access token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JoinMeetingResponse {
  pub guest_token: String,
  /// The session ends after the meeting (plus the configured grace time).
  pub expires_at: UnixMillis,
  pub meeting_id: MeetingId,
  /// The meeting chat; the guest is a member once admitted.
  pub conversation_id: ConversationId,
  pub user_id: UserId,
  pub lobby_state: LobbyState,
}

impl std::fmt::Debug for JoinMeetingResponse {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("JoinMeetingResponse")
      .field("guest_token", &"[redacted]")
      .field("meeting_id", &self.meeting_id)
      .field("lobby_state", &self.lobby_state)
      .finish()
  }
}

/// A guest waiting in the lobby.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LobbyEntry {
  pub user: UserSummary,
  pub requested_at: UnixMillis,
}

/// `GET /api/v1/meetings/{id}/lobby`: who waits for the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListLobbyResponse {
  pub entries: Vec<LobbyEntry>,
}

/// `GET /api/v1/meetings/{id}/lobby/me`: the caller's own place in the lobby.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LobbyStateResponse {
  pub state: LobbyState,
}

/// Result of `POST /api/internal/v1/meetings/replies`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeetingReplyResponse {
  /// Answers that matched an invitee of a known meeting; the rest was ignored.
  pub applied: u32,
}

fn default_true() -> bool {
  true
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn invitees_use_a_kind_tag() {
    let external: MeetingInvitee =
      serde_json::from_str(r#"{"kind":"external","email":"guest@example.com","name":"Guest"}"#).unwrap();
    assert_eq!(
      external,
      MeetingInvitee::External {
        email: "guest@example.com".into(),
        maybe_name: Some("Guest".into())
      }
    );
    assert!(serde_json::from_str::<MeetingInvitee>(r#"{"kind":"robot"}"#).is_err());
  }

  #[test]
  fn requests_default_to_a_lobby_and_no_invitees() {
    let request: CreateMeetingRequest =
      serde_json::from_str(r#"{"title":"Sync","starts_at":1000,"ends_at":2000}"#).unwrap();
    assert!(request.has_lobby && request.invitees.is_empty());
  }

  const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f";

  #[test]
  fn create_requests_carry_an_optional_idempotency_key() {
    // Older clients send no key.
    let old: CreateMeetingRequest =
      serde_json::from_str(r#"{"title":"Sync","starts_at":1000,"ends_at":2000}"#).unwrap();
    assert_eq!(old.maybe_idempotency_key, None);
    assert!(
      serde_json::to_value(&old)
        .unwrap()
        .get("idempotency_key")
        .is_none()
    );

    let key = IdempotencyKey::from_hex(KEY_HEX).unwrap();
    let request: CreateMeetingRequest = serde_json::from_str(&format!(
      r#"{{"title":"Sync","starts_at":1000,"ends_at":2000,"idempotency_key":"{KEY_HEX}"}}"#
    ))
    .unwrap();
    assert_eq!(request.maybe_idempotency_key, Some(key));
    assert_eq!(
      serde_json::to_value(&request).unwrap()["idempotency_key"],
      KEY_HEX
    );

    let instant = CreateInstantMeetingRequest {
      maybe_title: None,
      maybe_idempotency_key: Some(key),
    };
    assert_eq!(
      serde_json::to_string(&instant).unwrap(),
      format!(r#"{{"idempotency_key":"{KEY_HEX}"}}"#)
    );
    assert!(serde_json::from_str::<CreateInstantMeetingRequest>(r#"{"idempotency_key":"zz"}"#).is_err());

    let space: crate::CreateSpaceRequest = serde_json::from_str(&format!(
      r#"{{"name":"S","join_policy":"open","idempotency_key":"{KEY_HEX}"}}"#
    ))
    .unwrap();
    assert_eq!(space.maybe_idempotency_key, Some(key));
    let channel: crate::CreateChannelRequest = serde_json::from_str(&format!(
      r#"{{"name":"c","visibility":"private","idempotency_key":"{KEY_HEX}"}}"#
    ))
    .unwrap();
    assert_eq!(channel.maybe_idempotency_key, Some(key));
    let legacy: crate::CreateChannelRequest =
      serde_json::from_str(r#"{"name":"c","visibility":"private"}"#).unwrap();
    assert_eq!(legacy.maybe_idempotency_key, None);
  }

  #[test]
  fn statuses_tolerate_unknown_values() {
    let status: MeetingStatus = serde_json::from_str("\"postponed\"").unwrap();
    assert_eq!(status, MeetingStatus::Unknown("postponed".into()));
  }
}
