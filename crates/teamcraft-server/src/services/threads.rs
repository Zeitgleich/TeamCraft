//! Followed threads, the threads inbox and unread counts (PLAN.md §3).

use teamcraft_db::{StoredMessage, ThreadActivity, ThreadQuery};
use teamcraft_proto::{
  ConversationId, ConversationUnread, GatewayEvent, Id, ListThreadsQuery, ListThreadsResponse, MessageId,
  SetThreadReadRequest, ThreadId, ThreadSummary, UnreadResponse, UserId,
};

use super::access::load_conversation;
use super::convert::message_response;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Applies the automatic follow rules for a stored reply and tells the affected users' devices.
pub async fn record_reply(
  state: &AppState,
  root: &StoredMessage,
  reply: &StoredMessage,
) -> Result<(), ApiError> {
  let thread = ThreadId::from_snowflake(root.id.snowflake());
  let changed = state
    .database
    .record_thread_activity(&ThreadActivity {
      conversation: reply.conversation_id,
      thread,
      message: reply.id,
      sender: reply.sender_user_id,
      root_sender: root.sender_user_id,
      mentioned: &reply.mentioned_user_ids,
      follows_mentions_by_default: state.policy.get().threads.auto_follows_mentioned_threads,
    })
    .await?;
  announce(state, reply.conversation_id, thread, changed);
  Ok(())
}

/// Follows a thread; replies written before this moment count as read.
pub async fn follow_thread(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  thread: ThreadId,
) -> Result<(), ApiError> {
  require_thread(state, caller, conversation, thread).await?;
  state
    .database
    .set_thread_follow(caller.user.id, conversation, thread, true)
    .await?;
  announce(state, conversation, thread, vec![caller.user.id]);
  Ok(())
}

/// Unfollows a thread; later replies don't bring it back (only the user's own reply does).
pub async fn unfollow_thread(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  thread: ThreadId,
) -> Result<(), ApiError> {
  require_thread(state, caller, conversation, thread).await?;
  state
    .database
    .set_thread_follow(caller.user.id, conversation, thread, false)
    .await?;
  announce(state, conversation, thread, vec![caller.user.id]);
  Ok(())
}

/// Moves the caller's read position in a followed thread forward.
pub async fn set_thread_read(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  thread: ThreadId,
  request: SetThreadReadRequest,
) -> Result<(), ApiError> {
  require_thread(state, caller, conversation, thread).await?;
  let reply = state
    .database
    .get_message(conversation, request.message_id)
    .await?
    .filter(|message| message.maybe_thread_id == Some(thread))
    .ok_or_else(|| ApiError::InvalidRequest("not a reply in this thread".into()))?;
  if !state
    .database
    .set_thread_read_position(caller.user.id, conversation, thread, reply.id)
    .await?
  {
    return Err(ApiError::Conflict("the thread is not followed".into()));
  }
  announce(state, conversation, thread, vec![caller.user.id]);
  Ok(())
}

/// The caller's followed threads with unread counts, most recently active first.
pub async fn list_threads(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: ListThreadsQuery,
) -> Result<ListThreadsResponse, ApiError> {
  let limits = &state.settings.limits;
  let limit = query
    .maybe_limit
    .unwrap_or(limits.default_page_size)
    .clamp(1, limits.max_page_size);
  let mut rows = state
    .database
    .list_followed_threads(
      caller.user.id,
      &ThreadQuery {
        unread_only: query.unread_only,
        maybe_before: query.maybe_before,
        limit: i64::from(limit),
      },
    )
    .await?;
  let has_more = rows.len() > limit as usize;
  rows.truncate(limit as usize);

  let mut threads = Vec::with_capacity(rows.len());
  for row in rows {
    let maybe_last_reply = state
      .database
      .get_message(row.conversation, row.last_reply)
      .await?
      .map(|message| message_response(&message));
    threads.push(ThreadSummary {
      conversation_id: row.conversation,
      thread_id: row.thread,
      follow_reason: row.reason.into(),
      reply_count: row.reply_count,
      unread_count: row.unread_count,
      mention_count: row.mention_count,
      maybe_last_reply,
    });
  }
  Ok(ListThreadsResponse { threads, has_more })
}

/// Unread and mention counts of all conversations and followed threads.
pub async fn get_unread(state: &AppState, caller: &AuthenticatedUser) -> Result<UnreadResponse, ApiError> {
  let conversations = state
    .database
    .conversation_unread_counts(caller.user.id, state.settings.limits.max_unread_counted)
    .await?
    .into_iter()
    .map(|row| ConversationUnread {
      conversation_id: row.conversation,
      unread_count: row.unread_count,
      mention_count: row.mention_count,
    })
    .collect();
  let totals = state.database.thread_unread_totals(caller.user.id).await?;
  Ok(UnreadResponse {
    conversations,
    unread_thread_count: totals.unread_thread_count,
    thread_mention_count: totals.mention_count,
  })
}

/// The caller must be a member and `thread` must be the root message of a thread in the conversation.
async fn require_thread(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  thread: ThreadId,
) -> Result<(), ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden("only members can follow threads".into()));
  }
  state
    .database
    .get_message(conversation, MessageId::from_snowflake(thread.snowflake()))
    .await?
    .filter(|root| root.maybe_thread_id.is_none())
    .ok_or_else(|| ApiError::InvalidRequest("not the root message of a thread".into()))?;
  Ok(())
}

fn announce(state: &AppState, conversation: ConversationId, thread: ThreadId, users: Vec<UserId>) {
  state.hub.publish(
    users,
    &GatewayEvent::ThreadUpdate {
      conversation_id: conversation,
      thread_id: thread,
    },
  );
}
