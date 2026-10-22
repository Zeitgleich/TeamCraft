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
  ConversationId, DeviceActivity, DeviceId, ErrorBody, MessageResponse, PresenceState, SpaceId, UserId,
  UserSummary,
};

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
  /// The user is typing in a conversation.
  Typing {
    conversation_id: ConversationId,
  },
  /// The device became active or idle.
  SetActivity {
    activity: DeviceActivity,
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
  },
  PresenceUpdate {
    user_id: UserId,
    state: PresenceState,
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
      Self::Typing { conversation_id } => f
        .debug_struct("Typing")
        .field("conversation_id", conversation_id)
        .finish(),
      Self::SetActivity { activity } => f.debug_struct("SetActivity").field("activity", activity).finish(),
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
  fn presence_update_wire_format() {
    let event = GatewayEvent::PresenceUpdate {
      user_id: UserId::from_snowflake(Snowflake::new(2).unwrap()),
      state: PresenceState::DoNotDisturb,
    };
    assert_eq!(
      serde_json::to_string(&event).unwrap(),
      r#"{"type":"presence_update","user_id":"2","state":"dnd"}"#
    );
  }
}
