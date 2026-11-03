//! WebSocket gateway messages (`/api/v1/gateway`), JSON text frames.
//!
//! Flow: the server sends `hello`; the client answers with `identify` (new session) or `resume`
//! (continue after a dropped connection). Every `event` carries a sequence number that increases by
//! one per session, so a client that reconnects with its last seen number gets exactly the events
//! it missed, or `session_invalid` and must refetch state over REST. The token travels in the first
//! frame, never in the URL (security.md).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
  AdminJoinRequestChange, CallEndCause, CallId, CallRosterEntry, ConversationId, CustomStatus,
  DeviceActivity, DeviceId, DeviceLinkClosedReason, DeviceLinkRequest, ErrorBody, LobbyState, MeetingId,
  MessageId, MessageResponse, MlsMessageResponse, PresenceState, RecoveryRequestId, SpaceId,
  SystemEventResponse, ThreadId, UnixMillis, UserGroupId, UserId, UserSummary, WorkspaceLogKind,
};

/// How long a client waits before it reports that the user stopped being attentive, so the server
/// learns coarse transitions, not a timeline of every glance away.
pub const ATTENTION_AWAY_DELAY_SECS: u64 = 60;

/// Frames sent by the client.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayClientMessage {
  /// Starts a new session. `token` is the OIDC access token.
  Identify {
    token: String,
    device_id: DeviceId,
    #[serde(default = "default_activity")]
    activity: DeviceActivity,
  },
  /// Continues session `session_id`; the server replays events after `last_seq`.
  Resume {
    token: String,
    session_id: String,
    last_seq: u64,
    device_id: DeviceId,
  },
  Heartbeat,
  /// The user is typing in a conversation, or with `thread_id` a reply in one of its threads (#882;
  /// older clients leave it out and older servers ignore it).
  Typing {
    conversation_id: ConversationId,
    #[serde(rename = "thread_id", default, skip_serializing_if = "Option::is_none")]
    maybe_thread_id: Option<ThreadId>,
  },
  /// The device became active or idle.
  SetActivity {
    activity: DeviceActivity,
  },
  /// Whether the user is looking at this desktop or web session right now: window focused and
  /// visible, recent input, screen not locked (notifications.md, "Attention"). Sent on changes only
  /// ("away" after [`ATTENTION_AWAY_DELAY_SECS`]) and only where the user's notification settings say
  /// `uses_desktop_attention`; the session's heartbeats keep an attentive report alive.
  Attention {
    is_attentive: bool,
  },
  #[serde(other)]
  Unknown,
}

/// Frames sent by the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayServerMessage {
  Hello {
    heartbeat_interval_ms: u64,
  },
  Ready {
    session_id: String,
    user: UserSummary,
  },
  Resumed {
    replayed: u64,
  },
  /// The session can't be resumed (expired or unknown); identify again and refetch state.
  SessionInvalid,
  HeartbeatAck,
  Event {
    seq: u64,
    event: GatewayEvent,
  },
  Error {
    error: ErrorBody,
  },
  #[serde(other)]
  Unknown,
}

/// Things that happened that the user may see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayEvent {
  MessageCreate {
    message: MessageResponse,
  },
  /// A commit, proposal, welcome or application message of the conversation's MLS group.
  MlsMessage {
    message: MlsMessageResponse,
  },
  /// A conversation the user is in (or just joined) changed; refetch it.
  ConversationUpdate {
    conversation_id: ConversationId,
  },
  MembershipAdd {
    conversation_id: ConversationId,
    user: UserSummary,
  },
  MembershipRemove {
    conversation_id: ConversationId,
    user_id: UserId,
  },
  /// A direct message was deleted with a deleted account (admin.md §3.2.1): the conversation is
  /// gone on the server; devices drop it with its local history.
  ConversationDelete {
    conversation_id: ConversationId,
  },
  /// The signed roster or policy log has a new entry; fetch entries after the known version.
  WorkspaceUpdate {
    kind: WorkspaceLogKind,
    version: u64,
  },
  /// A channel's signed role log has a new entry (issue #380); fetch entries after the known
  /// version. Sent to the channel's members.
  ChannelRolesUpdate {
    conversation_id: ConversationId,
    version: u64,
  },
  /// A meeting was created, changed, cancelled or answered; refetch `GET /meetings/{id}`. Sent to
  /// the organizer and the invited users.
  MeetingUpdate {
    meeting_id: MeetingId,
  },
  /// A guest waits in the lobby of a meeting. Sent to the meeting's hosts.
  LobbyJoin {
    meeting_id: MeetingId,
    user: UserSummary,
  },
  /// The host admitted or denied the guest. Sent to the guest.
  LobbyDecision {
    meeting_id: MeetingId,
    state: LobbyState,
  },
  /// Someone changed their profile or avatar. Sent to people who share a conversation with them.
  ProfileUpdate {
    user_id: UserId,
  },
  /// An admin asked to join a private channel, or the request was decided, withdrawn or ran out
  /// (admin.md §2.4). Sent to the channel's owners and the admin; they fetch
  /// `GET /admin-join-requests` again.
  AdminJoinRequestUpdate {
    conversation_id: ConversationId,
    user_id: UserId,
    change: AdminJoinRequestChange,
  },
  /// A request for organization recovery was made or moved on. Sent to the admins, the person
  /// and the approvers; they fetch the request.
  OrgRecoveryUpdate {
    request_id: RecoveryRequestId,
  },
  /// The user's key bag changed (new version). Sent to the user's own devices, which fetch it so
  /// pins and the workspace root stay in sync (security-review A2).
  KeyBagUpdate {
    version: u64,
  },
  /// The announcement banner changed or ended. Clients fetch it again.
  AnnouncementUpdate,
  /// Something happened to a conversation (a member joined, the channel was archived, ...). Sent
  /// to its members; shown as a system message.
  SystemEvent {
    event: SystemEventResponse,
  },
  /// A user group was created, changed, deleted or got other members. Sent to everyone online.
  UserGroupUpdate {
    group_id: UserGroupId,
  },
  /// A guest's membership of a space ran out and they were removed. Sent to the sponsor and the
  /// space's admins.
  GuestExpired {
    space_id: SpaceId,
    user: UserSummary,
  },
  /// A member acknowledged an urgent message. Sent to the message's sender and to the member's
  /// other devices.
  MessageAck {
    conversation_id: ConversationId,
    message_id: MessageId,
    user_id: UserId,
  },
  /// A member of a DM or group DM read up to `message_id`. Sent only to members who send receipts
  /// themselves, and only if the reader does.
  ReadReceipt {
    conversation_id: ConversationId,
    user_id: UserId,
    message_id: MessageId,
  },
  /// The user's unread and mention counts of a conversation changed because their read position
  /// moved (on any of their devices). Sent only to that user. New messages raise the counts without
  /// this event: clients count them from `message_create` (notifications.md, "Unread counts").
  UnreadUpdate {
    conversation_id: ConversationId,
    unread_count: u32,
    mention_count: u32,
  },
  /// The user's notification settings (DND schedule and overrides, push mode) changed on another
  /// device; refetch `GET /me/notification-settings`. Sent only to that user.
  NotificationSettingsUpdate,
  /// The user's thread follow or thread read position changed (on another device or by an
  /// automatic follow); refetch `GET /threads` or `GET /unread`. Sent only to that user.
  ThreadUpdate {
    conversation_id: ConversationId,
    thread_id: ThreadId,
  },
  /// The first participant joined: a call is running in the conversation.
  CallStart {
    conversation_id: ConversationId,
    call_id: CallId,
    started_by: UserId,
    started_at: UnixMillis,
    is_ringing: bool,
    participants: Vec<CallRosterEntry>,
  },
  /// Someone joined or left, or the ringing stopped.
  CallUpdate {
    conversation_id: ConversationId,
    call_id: CallId,
    is_ringing: bool,
    participants: Vec<CallRosterEntry>,
  },
  /// The call is over: the last participant left, or one side of a 1:1 call did (`cause`).
  CallEnd {
    conversation_id: ConversationId,
    call_id: CallId,
    /// Absent from older servers, and when the last participant left.
    #[serde(default, skip_serializing_if = "CallEndCause::is_last_left")]
    cause: CallEndCause,
  },
  /// Somebody added people to the DM or group DM call `call_id`: the call moves into the meeting
  /// chat `to_conversation_id`. Devices in the call leave it and join the meeting's call; the DM
  /// itself stays as it was. Sent to the members of `conversation_id`.
  CallMove {
    conversation_id: ConversationId,
    call_id: CallId,
    to_conversation_id: ConversationId,
    moved_by: UserId,
  },
  /// Sent to all devices of the other members of a DM or group DM when a call starts there. Stop
  /// ringing when a `call_update` with `is_ringing: false` or `call_end` arrives.
  CallRing {
    conversation_id: ConversationId,
    call_id: CallId,
    from_user_id: UserId,
  },
  /// A user registered a new device. Members of shared conversations add it to their MLS groups
  /// (fetch `GET /users/{id}/devices`); the user's other devices also learn about it.
  DeviceAdd {
    user_id: UserId,
    device_id: DeviceId,
  },
  /// A new device of the user asks to be linked (e2ee.md §5). Sent to the user's devices; the first
  /// one that takes it up joins the relay, and the others get `device_link_closed`.
  DeviceLinkRequested {
    request: DeviceLinkRequest,
  },
  /// A pending link request is gone: drop its prompt.
  DeviceLinkClosed {
    request_id: String,
    reason: DeviceLinkClosedReason,
  },
  /// A device of a user was removed (lost phone, deactivation); members commit its removal from their groups.
  DeviceRemove {
    user_id: UserId,
    device_id: DeviceId,
  },
  SpaceUpdate {
    space_id: SpaceId,
  },
  SpaceMemberAdd {
    space_id: SpaceId,
    user: UserSummary,
  },
  SpaceMemberRemove {
    space_id: SpaceId,
    user_id: UserId,
  },
  TypingStart {
    conversation_id: ConversationId,
    user_id: UserId,
    /// The thread the reply is typed in; absent for the conversation itself (and from older
    /// clients, whose typing then shows in the conversation).
    #[serde(rename = "thread_id", default, skip_serializing_if = "Option::is_none")]
    maybe_thread_id: Option<ThreadId>,
  },
  PresenceUpdate {
    user_id: UserId,
    state: PresenceState,
    /// The user's custom status; absent if none (or just cleared).
    #[serde(rename = "status", default, skip_serializing_if = "Option::is_none")]
    maybe_status: Option<CustomStatus>,
  },
  #[serde(other)]
  Unknown,
}

// The token must not end up in logs.
impl fmt::Debug for GatewayClientMessage {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Identify {
        device_id, activity, ..
      } => f
        .debug_struct("Identify")
        .field("token", &"[redacted]")
        .field("device_id", device_id)
        .field("activity", activity)
        .finish(),
      Self::Resume {
        session_id,
        last_seq,
        device_id,
        ..
      } => f
        .debug_struct("Resume")
        .field("token", &"[redacted]")
        .field("session_id", session_id)
        .field("last_seq", last_seq)
        .field("device_id", device_id)
        .finish(),
      Self::Heartbeat => f.write_str("Heartbeat"),
      Self::Typing {
        conversation_id,
        maybe_thread_id,
      } => f
        .debug_struct("Typing")
        .field("conversation_id", conversation_id)
        .field("maybe_thread_id", maybe_thread_id)
        .finish(),
      Self::SetActivity { activity } => f.debug_struct("SetActivity").field("activity", activity).finish(),
      Self::Attention { is_attentive } => f
        .debug_struct("Attention")
        .field("is_attentive", is_attentive)
        .finish(),
      Self::Unknown => f.write_str("Unknown"),
    }
  }
}

fn default_activity() -> DeviceActivity {
  DeviceActivity::Active
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Id, Snowflake};

  fn conversation() -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(7).unwrap())
  }

  #[test]
  fn client_messages_have_stable_wire_format() {
    let json = serde_json::to_string(&GatewayClientMessage::Typing {
      conversation_id: conversation(),
      maybe_thread_id: None,
    })
    .unwrap();
    assert_eq!(json, r#"{"type":"typing","conversation_id":"7"}"#);
    assert_eq!(
      serde_json::to_string(&GatewayClientMessage::Heartbeat).unwrap(),
      r#"{"type":"heartbeat"}"#
    );
    let identify: GatewayClientMessage =
      serde_json::from_str(r#"{"type":"identify","token":"t","device_id":"3"}"#).unwrap();
    assert!(matches!(
      identify,
      GatewayClientMessage::Identify {
        activity: DeviceActivity::Active,
        ..
      }
    ));
  }

  #[test]
  fn typing_in_a_thread_names_it_and_stays_readable_without() {
    let thread = ThreadId::from_snowflake(Snowflake::new(9).unwrap());
    let json = serde_json::to_string(&GatewayClientMessage::Typing {
      conversation_id: conversation(),
      maybe_thread_id: Some(thread),
    })
    .unwrap();
    assert_eq!(json, r#"{"type":"typing","conversation_id":"7","thread_id":"9"}"#);
    let event = GatewayEvent::TypingStart {
      conversation_id: conversation(),
      user_id: UserId::from_snowflake(Snowflake::new(2).unwrap()),
      maybe_thread_id: Some(thread),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert_eq!(
      json,
      r#"{"type":"typing_start","conversation_id":"7","user_id":"2","thread_id":"9"}"#
    );
    assert_eq!(serde_json::from_str::<GatewayEvent>(&json).unwrap(), event);
    // From an older server or client: no thread, typing in the conversation.
    let old: GatewayEvent =
      serde_json::from_str(r#"{"type":"typing_start","conversation_id":"7","user_id":"2"}"#).unwrap();
    assert!(matches!(
      old,
      GatewayEvent::TypingStart {
        maybe_thread_id: None,
        ..
      }
    ));
  }

  #[test]
  fn attention_reports_have_a_stable_wire_format() {
    assert_eq!(
      serde_json::to_string(&GatewayClientMessage::Attention { is_attentive: true }).unwrap(),
      r#"{"type":"attention","is_attentive":true}"#
    );
    assert_eq!(
      serde_json::to_string(&GatewayEvent::NotificationSettingsUpdate).unwrap(),
      r#"{"type":"notification_settings_update"}"#
    );
  }

  #[test]
  fn conversation_delete_has_a_stable_wire_format() {
    let event = GatewayEvent::ConversationDelete {
      conversation_id: conversation(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert_eq!(json, r#"{"type":"conversation_delete","conversation_id":"7"}"#);
    assert_eq!(serde_json::from_str::<GatewayEvent>(&json).unwrap(), event);
  }

  #[test]
  fn channel_roles_update_has_a_stable_wire_format() {
    let event = GatewayEvent::ChannelRolesUpdate {
      conversation_id: conversation(),
      version: 3,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert_eq!(
      json,
      r#"{"type":"channel_roles_update","conversation_id":"7","version":3}"#
    );
    assert_eq!(serde_json::from_str::<GatewayEvent>(&json).unwrap(), event);
  }

  #[test]
  fn unknown_client_message_is_tolerated() {
    let message: GatewayClientMessage = serde_json::from_str(r#"{"type":"teleport","x":1}"#).unwrap();
    assert!(matches!(message, GatewayClientMessage::Unknown));
  }

  #[test]
  fn tokens_are_redacted_in_debug_output() {
    let message = GatewayClientMessage::Identify {
      token: "secret-token".into(),
      device_id: DeviceId::from_snowflake(Snowflake::new(1).unwrap()),
      activity: DeviceActivity::Idle,
    };
    assert!(!format!("{message:?}").contains("secret-token"));
  }

  #[test]
  fn server_events_round_trip_and_tolerate_unknown_types() {
    let event = GatewayServerMessage::Event {
      seq: 5,
      event: GatewayEvent::TypingStart {
        conversation_id: conversation(),
        user_id: UserId::from_snowflake(Snowflake::new(9).unwrap()),
        maybe_thread_id: None,
      },
    };
    let json = serde_json::to_string(&event).unwrap();
    assert_eq!(
      json,
      r#"{"type":"event","seq":5,"event":{"type":"typing_start","conversation_id":"7","user_id":"9"}}"#
    );
    assert_eq!(
      serde_json::from_str::<GatewayServerMessage>(&json).unwrap(),
      event
    );

    let future: GatewayServerMessage =
      serde_json::from_str(r#"{"type":"event","seq":6,"event":{"type":"poll_vote","poll":1}}"#).unwrap();
    assert_eq!(
      future,
      GatewayServerMessage::Event {
        seq: 6,
        event: GatewayEvent::Unknown
      }
    );
    let future_frame: GatewayServerMessage = serde_json::from_str(r#"{"type":"maintenance"}"#).unwrap();
    assert_eq!(future_frame, GatewayServerMessage::Unknown);
  }

  #[test]
  fn call_move_has_a_stable_wire_format() {
    let event = GatewayEvent::CallMove {
      conversation_id: conversation(),
      call_id: CallId::from_snowflake(Snowflake::new(8).unwrap()),
      to_conversation_id: ConversationId::from_snowflake(Snowflake::new(10).unwrap()),
      moved_by: UserId::from_snowflake(Snowflake::new(9).unwrap()),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert_eq!(
      json,
      r#"{"type":"call_move","conversation_id":"7","call_id":"8","to_conversation_id":"10","moved_by":"9"}"#
    );
    assert_eq!(serde_json::from_str::<GatewayEvent>(&json).unwrap(), event);
  }

  #[test]
  fn admin_join_request_update_has_a_stable_wire_format() {
    let event = GatewayEvent::AdminJoinRequestUpdate {
      conversation_id: conversation(),
      user_id: UserId::from_snowflake(Snowflake::new(9).unwrap()),
      change: AdminJoinRequestChange::Requested,
    };
    let json =
      r#"{"type":"admin_join_request_update","conversation_id":"7","user_id":"9","change":"requested"}"#;
    assert_eq!(serde_json::to_string(&event).unwrap(), json);
    assert_eq!(serde_json::from_str::<GatewayEvent>(json).unwrap(), event);
  }

  #[test]
  fn unread_update_has_a_stable_wire_format() {
    let event = GatewayEvent::UnreadUpdate {
      conversation_id: conversation(),
      unread_count: 3,
      mention_count: 1,
    };
    let json = r#"{"type":"unread_update","conversation_id":"7","unread_count":3,"mention_count":1}"#;
    assert_eq!(serde_json::to_string(&event).unwrap(), json);
    assert_eq!(serde_json::from_str::<GatewayEvent>(json).unwrap(), event);
    // Older clients that don't know the event yet skip it.
    let unknown: GatewayEvent =
      serde_json::from_str(r#"{"type":"unread_update_v2","conversation_id":"7"}"#).unwrap();
    assert_eq!(unknown, GatewayEvent::Unknown);
  }

  #[test]
  fn presence_update_carries_the_custom_status_when_there_is_one() {
    let event = GatewayEvent::PresenceUpdate {
      user_id: UserId::from_snowflake(Snowflake::new(2).unwrap()),
      state: PresenceState::Online,
      maybe_status: Some(CustomStatus {
        maybe_emoji: Some("🌴".into()),
        maybe_text: Some("Vacation".into()),
        maybe_expires_at: Some(crate::UnixMillis(1000)),
        maybe_activity: None,
      }),
    };
    assert_eq!(
      serde_json::to_string(&event).unwrap(),
      r#"{"type":"presence_update","user_id":"2","state":"online","status":{"emoji":"🌴","text":"Vacation","expires_at":1000}}"#
    );
    assert_eq!(
      serde_json::from_str::<GatewayEvent>(&serde_json::to_string(&event).unwrap()).unwrap(),
      event
    );
    let old: GatewayEvent =
      serde_json::from_str(r#"{"type":"presence_update","user_id":"2","state":"away"}"#).unwrap();
    assert!(matches!(
      old,
      GatewayEvent::PresenceUpdate {
        maybe_status: None,
        ..
      }
    ));
  }

  #[test]
  fn presence_update_wire_format() {
    let event = GatewayEvent::PresenceUpdate {
      user_id: UserId::from_snowflake(Snowflake::new(2).unwrap()),
      state: PresenceState::DoNotDisturb,
      maybe_status: None,
    };
    assert_eq!(
      serde_json::to_string(&event).unwrap(),
      r#"{"type":"presence_update","user_id":"2","state":"dnd"}"#
    );
  }
}
