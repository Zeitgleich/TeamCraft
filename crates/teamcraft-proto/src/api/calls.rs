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

/// Body of the SFU's reports `POST /api/internal/v1/calls/participants/{joined,left}`. Internal:
/// authenticated with the SFU's registration token, not with a user token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SfuParticipantReport {
  pub call_id: CallId,
  pub user_id: UserId,
  pub device_id: DeviceId,
}
