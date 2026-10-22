//! Call history (PLAN.md §6).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{CallId, ConversationId, UnixMillis, UserId};

/// One call of a conversation the caller belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CallHistoryEntry {
  pub call_id: CallId,
  pub conversation_id: ConversationId,
  #[serde(rename = "started_by", default, skip_serializing_if = "Option::is_none")]
  pub maybe_started_by: Option<UserId>,
  pub started_at: UnixMillis,
  /// Absent while the call goes on.
  #[serde(rename = "ended_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_ended_at: Option<UnixMillis>,
  /// Everybody who joined at some point.
  pub participants: Vec<UserId>,
  /// The call rang the caller and they never joined.
  pub is_missed: bool,
}

/// Query of `GET /api/v1/calls/history`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListCallHistoryQuery {
  /// Continue with calls older than this one (newest first).
  #[serde(rename = "before", default, skip_serializing_if = "Option::is_none")]
  pub maybe_before: Option<CallId>,
  #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none")]
  pub maybe_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListCallHistoryResponse {
  pub calls: Vec<CallHistoryEntry>,
  pub has_more: bool,
}
