use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{CallId, DeviceId, UnixMillis};

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
