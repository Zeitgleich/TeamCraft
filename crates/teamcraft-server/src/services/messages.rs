use std::collections::HashSet;

use teamcraft_db::{HistoryQuery, NewMessage};
use teamcraft_proto::{
  ConversationId, GatewayEvent, HistoryPolicy, Id, ListMessagesQuery, ListMessagesResponse, MessageId,
  MessageResponse, Permission, SendMessageRequest,
};

use super::access::{is_guest, load_conversation, require};
use super::convert::message_response;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

const MAX_CONTENT_KEY_ID_BYTES: usize = 64;

/// The stored message and whether this call created it (a repeated client ID returns the old one).
pub struct Sent {
  pub message: MessageResponse,
  pub is_new: bool,
}

/// Stores an opaque ciphertext and fans it out to the conversation's members over the gateway.
/// The server never sees plaintext (ARCHITECTURE.md §4.3).
pub async fn send_message(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  request: SendMessageRequest,
) -> Result<Sent, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  require(state, caller, Permission::CreatePost, &access.target())?;

  let limits = &state.settings.limits;
  if request.ciphertext.is_empty() {
    return Err(ApiError::InvalidRequest("ciphertext must not be empty".into()));
  }
  if request.ciphertext.len() > limits.max_ciphertext_bytes {
    return Err(ApiError::PayloadTooLarge(format!(
      "ciphertext exceeds {} bytes",
      limits.max_ciphertext_bytes
    )));
  }
  if request.content_key_id.is_empty() || request.content_key_id.len() > MAX_CONTENT_KEY_ID_BYTES {
    return Err(ApiError::InvalidRequest(
      "content_key_id has an invalid size".into(),
    ));
  }
  if request.mentioned_user_ids.len() > limits.max_mentions_per_message {
    return Err(ApiError::InvalidRequest("too many mentions".into()));
  }
  if !state
    .database
    .is_active_device_of(caller.user.id, request.device_id)
    .await?
  {
    return Err(ApiError::device_not_found());
  }

  let members: HashSet<_> = state
    .database
    .conversation_member_ids(conversation)
    .await?
    .into_iter()
    .collect();
  if let Some(stranger) = request
    .mentioned_user_ids
    .iter()
    .find(|user| !members.contains(user))
  {
    return Err(ApiError::InvalidRequest(format!(
      "mentioned user {stranger} is not in the conversation"
    )));
  }
  if let Some(thread) = request.maybe_thread_id
    && state
      .database
      .get_message(conversation, MessageId::from_snowflake(thread.snowflake()))
      .await?
      .is_none()
  {
    return Err(ApiError::InvalidRequest(
      "thread root is not a message of this conversation".into(),
    ));
  }

  let mut mentioned = request.mentioned_user_ids.clone();
  mentioned.sort_unstable();
  mentioned.dedup();
  let outcome = state
    .database
    .store_message(&NewMessage {
      id: state.next_id()?,
      client_message_id: request.client_message_id,
      conversation_id: conversation,
      sender_user_id: caller.user.id,
      sender_device_id: request.device_id,
      maybe_thread_id: request.maybe_thread_id,
      mentioned_user_ids: &mentioned,
      is_urgent: request.is_urgent,
      content_key_id: request.content_key_id.as_slice(),
      ciphertext: request.ciphertext.as_slice(),
    })
    .await?;

  let response = message_response(&outcome.message);
  if outcome.is_new {
    state.hub.publish(
      members,
      &GatewayEvent::MessageCreate {
        message: response.clone(),
      },
    );
    tokio::spawn(crate::push::routing::notify_message(
      state.clone(),
      crate::push::routing::NewMessageEvent {
        conversation,
        kind: access.view.conversation.kind.clone(),
        message: outcome.message.id,
        sender: caller.user.id,
        mentioned: outcome.message.mentioned_user_ids.clone(),
        is_urgent: outcome.message.is_urgent,
      },
    ));
  }
  Ok(Sent {
    message: response,
    is_new: outcome.is_new,
  })
}

/// One page of history, newest first (or oldest first with `after`). Guests and channels with the
/// `from_join` policy only see messages created after the caller joined.
pub async fn list_messages(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  query: ListMessagesQuery,
) -> Result<ListMessagesResponse, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  let Some(membership) = &access.view.maybe_membership else {
    return Err(ApiError::Forbidden("only members can read messages".into()));
  };
  if query.maybe_before.is_some() && query.maybe_after.is_some() {
    return Err(ApiError::InvalidRequest("use either before or after".into()));
  }

  let limits = &state.settings.limits;
  let limit = query
    .maybe_limit
    .unwrap_or(limits.default_page_size)
    .clamp(1, limits.max_page_size);
  let sees_all_history = access.view.conversation.history_policy == HistoryPolicy::Full && !is_guest(caller);
  let mut messages = state
    .database
    .list_messages(&HistoryQuery {
      conversation,
      maybe_before: query.maybe_before,
      maybe_after: query.maybe_after,
      maybe_not_before: if sees_all_history {
        None
      } else {
        Some(membership.joined_floor)
      },
      limit: i64::from(limit),
    })
    .await?;

  let has_more = messages.len() > limit as usize;
  messages.truncate(limit as usize);
  Ok(ListMessagesResponse {
    messages: messages.iter().map(message_response).collect(),
    has_more,
  })
}
