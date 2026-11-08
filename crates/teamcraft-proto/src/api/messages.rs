use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{Base64Bytes, ClientMessageId, ConversationId, DeviceId, MessageId, ThreadId, UserId};

/// Default of the server's `[limits] max_mentions_per_message`: IDs a message may name in
/// `mentioned_user_ids`. A device caps a group mention at it instead of having the message refused.
pub const DEFAULT_MAX_MENTIONS_PER_MESSAGE: usize = 200;

/// Default of `[limits] max_mentions_without_group_mentions`: IDs a message may name without
/// `use_group_mentions` in its conversation. All a server that can't read the message can do about
/// that permission (docs/e2ee.md §4.4).
pub const DEFAULT_MAX_MENTIONS_WITHOUT_GROUP_MENTIONS: usize = 10;

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
  /// The text addresses the whole conversation (`@channel`, `@here`, `@all`, see
  /// [`addresses_everyone`]): members who let those notify them in this conversation are pushed
  /// as for a mention (notifications.md, "Per-conversation filters").
  #[serde(default, skip_serializing_if = "is_false")]
  pub is_channel_mention: bool,
}

/// Words that address the whole conversation, written with a leading `@`.
pub const CHANNEL_MENTIONS: [&str; 3] = ["channel", "here", "all"];

/// Whether a message text addresses the whole conversation: `@channel`, `@here` or `@all` as a word
/// of its own. Senders set [`SendMessageRequest::is_channel_mention`] from it; devices use it for
/// their own mention rules.
pub fn addresses_everyone(body: &str) -> bool {
  let lower = body.to_lowercase();
  let words: Vec<&str> = lower
    .split(|c: char| !c.is_alphanumeric() && c != '@' && c != '_' && c != '-')
    .collect();
  CHANNEL_MENTIONS
    .iter()
    .any(|mention| words.contains(&format!("@{mention}").as_str()))
}

pub(crate) fn is_false(value: &bool) -> bool {
  !*value
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::Id;

  #[test]
  fn channel_mentions_are_whole_words() {
    assert!(addresses_everyone("@channel standup in 5"));
    assert!(addresses_everyone("hi @Here!"));
    assert!(addresses_everyone("(@all)"));
    assert!(!addresses_everyone("mail me@channel.example"));
    assert!(!addresses_everyone("@channels are fun"));
    assert!(!addresses_everyone("channel here all"));
    assert!(!addresses_everyone(""));
  }

  #[test]
  fn the_channel_mention_bit_is_left_out_when_unset() {
    let request = SendMessageRequest {
      client_message_id: ClientMessageId::from_bytes([1; 16]),
      device_id: DeviceId::from_snowflake(crate::Snowflake::new(2).unwrap()),
      content_key_id: Base64Bytes::new(Vec::new()),
      ciphertext: Base64Bytes::new(Vec::new()),
      maybe_thread_id: None,
      mentioned_user_ids: Vec::new(),
      is_urgent: false,
      is_channel_mention: false,
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(!json.contains("is_channel_mention"));
    // Older clients send no bit: it reads as unset.
    assert_eq!(
      serde_json::from_str::<SendMessageRequest>(&json).unwrap(),
      request
    );
    let set = SendMessageRequest {
      is_channel_mention: true,
      ..request
    };
    assert!(
      serde_json::to_string(&set)
        .unwrap()
        .contains(r#""is_channel_mention":true"#)
    );
  }
}
