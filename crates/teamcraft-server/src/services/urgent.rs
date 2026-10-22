//! Urgent messages: acknowledgements and the repeat schedule for urgent direct messages
//! (PLAN.md §5). The sender marks a message urgent; in a DM or group DM every recipient is pushed
//! again every `urgent_repeat_interval_secs` until they acknowledge it or `urgent_max_pushes` is
//! reached. The first push comes from the normal routing (`push::routing`).

use std::time::Duration;

use teamcraft_db::StoredMessage;
use teamcraft_proto::{
  ConversationId, ConversationKind, GatewayEvent, ListAcknowledgementsResponse, MessageAcknowledgement,
  MessageId,
};
use time::OffsetDateTime;

use super::access::load_conversation;
use super::convert::unix_millis;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::push::routing;
use crate::state::AppState;

/// Reminders handled per scheduler run; a busy instance catches up on the next run.
const BATCH_SIZE: i64 = 200;

/// Starts the repeat schedule for a freshly stored message if it is an urgent direct message.
pub async fn schedule_reminders(
  state: &AppState,
  kind: &ConversationKind,
  message: &StoredMessage,
  now: OffsetDateTime,
) -> Result<(), ApiError> {
  let settings = &state.settings.notifications;
  let is_direct = matches!(kind, ConversationKind::Dm | ConversationKind::Group);
  if !message.is_urgent || !is_direct || settings.urgent_max_pushes <= 1 {
    return Ok(());
  }
  let first_repeat_at = now + interval(state);
  state
    .database
    .schedule_urgent_reminders(
      message.conversation_id,
      message.id,
      message.sender_user_id,
      first_repeat_at,
    )
    .await?;
  Ok(())
}

/// Pushes every urgent direct message that is due again and returns how many recipients were handled.
pub async fn run_due(state: &AppState, now: OffsetDateTime) -> Result<usize, ApiError> {
  let max_pushes = i32::try_from(state.settings.notifications.urgent_max_pushes).unwrap_or(i32::MAX);
  let due = state
    .database
    .claim_due_reminders(now, interval(state), max_pushes, BATCH_SIZE)
    .await?;
  for reminder in &due {
    routing::notify_reminder(state, *reminder, now).await?;
  }
  Ok(due.len())
}

/// Confirms that the caller saw an urgent message; ends their reminders and tells the sender.
pub async fn acknowledge(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  message: MessageId,
) -> Result<(), ApiError> {
  let stored = urgent_message(state, caller, conversation, message).await?;
  if stored.sender_user_id == caller.user.id {
    return Err(ApiError::InvalidRequest(
      "a sender can't acknowledge their own message".into(),
    ));
  }
  if state
    .database
    .acknowledge_message(conversation, message, caller.user.id)
    .await?
  {
    state.hub.publish(
      [stored.sender_user_id, caller.user.id],
      &GatewayEvent::MessageAck {
        conversation_id: conversation,
        message_id: message,
        user_id: caller.user.id,
      },
    );
  }
  Ok(())
}

/// Who acknowledged an urgent message so far.
pub async fn list_acknowledgements(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  message: MessageId,
) -> Result<ListAcknowledgementsResponse, ApiError> {
  urgent_message(state, caller, conversation, message).await?;
  let acknowledgements = state
    .database
    .list_acknowledgements(conversation, message)
    .await?
    .into_iter()
    .map(|ack| MessageAcknowledgement {
      user_id: ack.user,
      acknowledged_at: unix_millis(ack.acknowledged_at),
    })
    .collect();
  Ok(ListAcknowledgementsResponse {
    message_id: message,
    acknowledgements,
  })
}

/// The message, if the caller is a member and it was sent as urgent.
async fn urgent_message(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  message: MessageId,
) -> Result<StoredMessage, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden(
      "only members can see acknowledgements".into(),
    ));
  }
  let stored = state
    .database
    .get_message(conversation, message)
    .await?
    .ok_or_else(|| ApiError::InvalidRequest("unknown message".into()))?;
  if !stored.is_urgent {
    return Err(ApiError::InvalidRequest("the message is not urgent".into()));
  }
  Ok(stored)
}

fn interval(state: &AppState) -> Duration {
  Duration::from_secs(state.settings.notifications.urgent_repeat_interval_secs)
}
