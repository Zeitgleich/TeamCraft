use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{UnixMillis, UserId};

/// A bot account (an integration bridge or another automated client).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BotResponse {
  /// The bot's user ID; add it to spaces and channels like any user.
  pub id: UserId,
  pub name: String,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
  pub created_at: UnixMillis,
  /// Revoked bots are deactivated and their token no longer works.
  pub is_revoked: bool,
}

/// `POST /api/v1/admin/bots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateBotRequest {
  pub name: String,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
}

/// The bot with its token, shown once: only a hash is stored.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BotTokenResponse {
  pub bot: BotResponse,
  /// Bearer token (`tcb_...`) for the REST API and the gateway.
  pub token: String,
}

impl std::fmt::Debug for BotTokenResponse {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("BotTokenResponse")
      .field("bot", &self.bot)
      .field("token", &"[redacted]")
      .finish()
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListBotsResponse {
  pub bots: Vec<BotResponse>,
}
