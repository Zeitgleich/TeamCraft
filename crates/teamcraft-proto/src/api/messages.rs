use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{Base64Bytes, ClientMessageId, ConversationId, DeviceId, MessageId, ThreadId, UserId};

/// `POST /api/v1/conversations/{id}/messages`. The server stores the ciphertext without reading
/// it; only routing metadata is visible (ARCHITECTURE.md §4.4). Re-sending the same
/// `client_message_id` returns the stored message instead of creating a second one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SendMessageRequest {
  pub client_message_id: ClientMessageId,
  pub device_id: DeviceId,
  /// Identifies the content key the ciphertext was encrypted with.
  pub content_key_id: Base64Bytes,
  pub ciphertext: Base64Bytes,
  #[serde(rename = "thread_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread_id: Option<ThreadId>,
  #[serde(default)]
  pub mentioned_user_ids: Vec<UserId>,
  /// Server-visible urgency bit for push routing.
  #[serde(default)]
  pub is_urgent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MessageResponse {
  /// Server-assigned, time-sortable; the order within a conversation.
  pub id: MessageId,
  pub client_message_id: ClientMessageId,
  pub conversation_id: ConversationId,
  pub sender_user_id: UserId,
  pub sender_device_id: DeviceId,
  #[serde(rename = "thread_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread_id: Option<ThreadId>,
  pub mentioned_user_ids: Vec<UserId>,
  pub is_urgent: bool,
  pub content_key_id: Base64Bytes,
  pub ciphertext: Base64Bytes,
}

/// Query of `GET /api/v1/conversations/{id}/messages`: newest first, paged by message ID.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListMessagesQuery {
  /// Only messages older than this ID.
  #[serde(rename = "before", default, skip_serializing_if = "Option::is_none")]
  pub maybe_before: Option<MessageId>,
  /// Only messages newer than this ID (oldest first when set; used to catch up after a gap).
  #[serde(rename = "after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_after: Option<MessageId>,
  #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none")]
  pub maybe_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListMessagesResponse {
  pub messages: Vec<MessageResponse>,
  /// More messages exist beyond this page in the direction of the query.
  pub has_more: bool,
}
