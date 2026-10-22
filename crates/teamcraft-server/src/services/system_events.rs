//! System events of conversations (admin.md §4): the audit log made visible to the people it
//! concerns. Metadata only — who did what to whom — never content.

use teamcraft_proto::{
  ConversationId, GatewayEvent, HistoryPolicy, Id, ListSystemEventsQuery, ListSystemEventsResponse,
  SystemEventKind, SystemEventResponse, UserId,
};
use tracing::warn;

use super::access::{is_guest, load_conversation};
use super::convert::unix_millis;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;
use teamcraft_db::{NewSystemEvent, StoredSystemEvent};

/// Writes the event and tells the members. A failure is logged: the action itself has happened.
pub async fn emit(
  state: &AppState,
  conversation: ConversationId,
  kind: SystemEventKind,
  maybe_actor: Option<UserId>,
  maybe_subject: Option<UserId>,
) {
  if let Err(error) = try_emit(state, conversation, kind, maybe_actor, maybe_subject).await {
    warn!(%error, conversation_id = %conversation, "writing a system event failed");
  }
}

async fn try_emit(
  state: &AppState,
  conversation: ConversationId,
  kind: SystemEventKind,
  maybe_actor: Option<UserId>,
  maybe_subject: Option<UserId>,
) -> Result<(), ApiError> {
  let stored = state
    .database
    .record_system_event(&NewSystemEvent {
      id: state.next_id()?,
      conversation,
      kind: kind.as_str(),
      maybe_actor,
      maybe_subject,
    })
    .await?;
  let mut audience = state.database.conversation_member_ids(conversation).await?;
  // Somebody who just left or was removed should still see why.
  audience.extend(maybe_subject);
  state.hub.publish(
    audience,
    &GatewayEvent::SystemEvent {
      event: response(&stored),
    },
  );
  Ok(())
}

/// The events of a conversation the caller is a member of, oldest first. Guests and members of
/// channels with limited history see only what happened after they joined.
pub async fn list_system_events(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  query: ListSystemEventsQuery,
) -> Result<ListSystemEventsResponse, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  let Some(membership) = &access.view.maybe_membership else {
    return Err(ApiError::conversation_not_found());
  };
  let limits = &state.settings.limits;
  let limit = query
    .maybe_limit
    .unwrap_or(limits.default_page_size)
    .clamp(1, limits.max_page_size);
  let sees_all = access.view.conversation.history_policy == HistoryPolicy::Full && !is_guest(caller);
  let floor = membership.joined_floor.snowflake();
  let mut events = state
    .database
    .list_system_events(conversation, query.maybe_after, i64::from(limit))
    .await?;
  let has_more = events.len() > limit as usize;
  events.truncate(limit as usize);
  Ok(ListSystemEventsResponse {
    events: events
      .iter()
      .filter(|event| sees_all || event.id.snowflake() >= floor)
      .map(response)
      .collect(),
    has_more,
  })
}

fn response(event: &StoredSystemEvent) -> SystemEventResponse {
  SystemEventResponse {
    id: event.id,
    conversation_id: event.conversation,
    kind: SystemEventKind::from(event.kind.clone()),
    maybe_actor_id: event.maybe_actor,
    maybe_subject_id: event.maybe_subject,
    created_at: unix_millis(event.created_at),
  }
}
