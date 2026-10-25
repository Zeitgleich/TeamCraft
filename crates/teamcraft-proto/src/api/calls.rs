use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{CallId, ConversationId, DeviceId, UnixMillis, UserId};

/// Body of `POST /api/v1/conversations/{conversation_id}/call/join-token`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateCallJoinTokenRequest {
  /// The caller's device that will join; it becomes the DAVE member identity.
  pub device_id: DeviceId,
}

/// A short-lived token for joining the conversation's call on the SFU.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CallJoinTokenResponse {
  pub call_id: CallId,
  /// Signed join token for `CallClientMessage::Join`.
  pub token: String,
  /// WebTransport URL of the SFU, e.g. `https://sfu.example.com:4433/call`.
  pub sfu_url: String,
  pub expires_at: UnixMillis,
  /// SHA-256s of the SFU's certificates (64 hex digits each) when they are short-lived self-signed
  /// ones (test and development setups): browsers accept them through `serverCertificateHashes`.
  /// Several while one certificate replaces another. Empty when the certificate chains to a trusted
  /// root. The SFU only forwards end-to-end encrypted media, so this decides reachability, not
  /// confidentiality.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub sfu_certificate_sha256: Vec<String>,
}

// Join tokens don't belong in logs.
impl std::fmt::Debug for CallJoinTokenResponse {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("CallJoinTokenResponse")
      .field("call_id", &self.call_id)
      .field("token", &"[redacted]")
      .field("sfu_url", &self.sfu_url)
      .field("expires_at", &self.expires_at)
      .finish()
  }
}

/// One device in a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CallRosterEntry {
  pub user_id: UserId,
  pub device_id: DeviceId,
}

/// Body of `POST /api/v1/conversations/{conversation_id}/call/move`: a DM or group DM call moves
/// into a meeting room because somebody added people to it (docs/meetings.md, "A call becomes a
/// meeting"). The caller is in the call and organizes the meeting; everybody in the call is told to
/// switch over (`call_move`), and the added people are rung in the meeting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MoveCallRequest {
  /// The meeting chat the call moves into.
  pub to_conversation_id: ConversationId,
  /// The added people: they ring in the meeting chat (members of it only).
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub ring_user_ids: Vec<UserId>,
}

/// `GET /api/v1/conversations/{id}/call`: the call that is running in the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CallStateResponse {
  pub call_id: CallId,
  pub conversation_id: ConversationId,
  pub started_by: UserId,
  pub started_at: UnixMillis,
  /// DMs and group DMs ring until someone else joins or the ring timeout passes.
  pub is_ringing: bool,
  pub participants: Vec<CallRosterEntry>,
}

/// `POST {sfu}/internal/v1/calls/control`: a meeting host's action, which the server has authorized,
/// forwarded to the SFU running the call. Internal: authenticated with the SFU registration token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SfuControlRequest {
  pub call_id: CallId,
  /// The host who acted (shown to participants).
  pub by_user_id: UserId,
  /// Flattened: `"action": "remove_participant", "user_id": "2"` sit next to `call_id`.
  #[serde(flatten)]
  pub action: SfuControlAction,
}

/// What a meeting host can do to a running call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SfuControlAction {
  /// Ask every participant to mute (they may unmute).
  MuteAll,
  /// Disconnect all of the user's devices; the SFU refuses them for the rest of the call.
  RemoveParticipant {
    user_id: UserId,
  },
  /// No new participants until unlocked.
  Lock,
  Unlock,
  /// Disconnect everyone.
  End,
  #[serde(other)]
  Unknown,
}

/// Body of the SFU's reports `POST /api/internal/v1/calls/participants/{joined,left}`. Internal:
/// authenticated with the SFU's registration token, not with a user token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SfuParticipantReport {
  pub call_id: CallId,
  pub user_id: UserId,
  pub device_id: DeviceId,
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Id, Snowflake};

  #[test]
  fn control_requests_have_a_stable_wire_format() {
    let request = SfuControlRequest {
      call_id: CallId::from_snowflake(Snowflake::new(7).unwrap()),
      by_user_id: UserId::from_snowflake(Snowflake::new(1).unwrap()),
      action: SfuControlAction::RemoveParticipant {
        user_id: UserId::from_snowflake(Snowflake::new(2).unwrap()),
      },
    };
    let json = serde_json::to_string(&request).unwrap();
    assert_eq!(
      json,
      r#"{"call_id":"7","by_user_id":"1","action":"remove_participant","user_id":"2"}"#
    );
    assert_eq!(serde_json::from_str::<SfuControlRequest>(&json).unwrap(), request);
    let unknown: SfuControlAction = serde_json::from_str(r#"{"action":"spotlight"}"#).unwrap();
    assert_eq!(unknown, SfuControlAction::Unknown);
  }

  #[test]
  fn move_requests_have_a_stable_wire_format() {
    let request = MoveCallRequest {
      to_conversation_id: ConversationId::from_snowflake(Snowflake::new(3).unwrap()),
      ring_user_ids: vec![UserId::from_snowflake(Snowflake::new(4).unwrap())],
    };
    let json = serde_json::to_string(&request).unwrap();
    assert_eq!(json, r#"{"to_conversation_id":"3","ring_user_ids":["4"]}"#);
    assert_eq!(serde_json::from_str::<MoveCallRequest>(&json).unwrap(), request);
    let nobody_rung: MoveCallRequest = serde_json::from_str(r#"{"to_conversation_id":"3"}"#).unwrap();
    assert!(nobody_rung.ring_user_ids.is_empty());
  }
}
