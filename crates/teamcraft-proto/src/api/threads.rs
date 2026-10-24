use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{ConversationId, MessageId, MessageResponse, ThreadId};

wire_enum! {
  /// Why a user follows a thread.
  pub enum ThreadFollowReason {
    /// The user wrote the root message and somebody replied.
    Started => "started",
    /// The user replied in the thread.
    Replied => "replied",
    /// The user was mentioned in the thread.
    Mentioned => "mentioned",
    /// The user followed the thread themselves.
    Manual => "manual",
  }
}

/// A followed thread in the threads inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ThreadSummary {
  pub conversation_id: ConversationId,
  /// The root message.
  pub thread_id: ThreadId,
  pub follow_reason: ThreadFollowReason,
  pub reply_count: u32,
  /// Replies by others after the user's thread read position.
  pub unread_count: u32,
  /// Unread replies that mention the user.
  pub mention_count: u32,
  /// The newest reply (ciphertext), for the preview.
  #[serde(rename = "last_reply", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_reply: Option<MessageResponse>,
}

/// Query of `GET /api/v1/threads`: followed threads, most recently active first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListThreadsQuery {
  /// Only threads with unread replies.
  #[serde(default)]
  pub unread_only: bool,
  /// Only threads whose newest reply is older than this message ID.
  #[serde(rename = "before", default, skip_serializing_if = "Option::is_none")]
  pub maybe_before: Option<MessageId>,
  #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none")]
  pub maybe_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListThreadsResponse {
  pub threads: Vec<ThreadSummary>,
  pub has_more: bool,
}

/// Body of `PUT /api/v1/conversations/{id}/threads/{thread_id}/read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetThreadReadRequest {
  /// The newest reply the user has seen.
  pub message_id: MessageId,
}

/// Unread state of one conversation (top-level messages; thread replies count in the threads). The
/// counts cover at most the newest `limits.max_unread_counted` (default 1000) unread messages: a count
/// at that value means "that many or more", and mentions are counted among those messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConversationUnread {
  pub conversation_id: ConversationId,
  pub unread_count: u32,
  /// Unread messages that mention the user.
  pub mention_count: u32,
}

/// Response of `GET /api/v1/unread`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UnreadResponse {
  /// Only conversations with something unread.
  pub conversations: Vec<ConversationUnread>,
  /// Followed threads with unread replies.
  pub unread_thread_count: u32,
  /// Unread thread replies that mention the user.
  pub thread_mention_count: u32,
  /// `limits.max_unread_counted`: the highest count the server reports for a conversation, so a
  /// client that counts new messages itself stops there too. Absent from servers before 1.0.1.
  #[serde(rename = "max_counted", default, skip_serializing_if = "Option::is_none")]
  pub maybe_max_counted: Option<u32>,
}
