//! Call history (PLAN.md §6): who called whom and when, for the people in the conversation.

use teamcraft_db::StoredCall;
use teamcraft_proto::{CallHistoryEntry, ListCallHistoryQuery, ListCallHistoryResponse};

use super::access::is_guest;
use super::convert::unix_millis;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// The caller's calls, newest first. A call that rang the caller and that they never joined is
/// marked as missed.
pub async fn list_call_history(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: ListCallHistoryQuery,
) -> Result<ListCallHistoryResponse, ApiError> {
  let limits = &state.settings.limits;
  let limit = query
    .maybe_limit
    .unwrap_or(limits.default_page_size)
    .clamp(1, limits.max_page_size);
  let mut calls = state
    .database
    .list_call_history(
      caller.user.id,
      query.maybe_before,
      i64::from(limit),
      !is_guest(caller),
    )
    .await?;
  let has_more = calls.len() > limit as usize;
  calls.truncate(limit as usize);
  Ok(ListCallHistoryResponse {
    calls: calls.iter().map(|call| entry(call, caller)).collect(),
    has_more,
  })
}

fn entry(call: &StoredCall, caller: &AuthenticatedUser) -> CallHistoryEntry {
  let did_join = call.participants.contains(&caller.user.id);
  let is_starter = call.maybe_started_by == Some(caller.user.id);
  CallHistoryEntry {
    call_id: call.call,
    conversation_id: call.conversation,
    maybe_started_by: call.maybe_started_by,
    started_at: unix_millis(call.started_at),
    maybe_ended_at: call.maybe_ended_at.map(unix_millis),
    participants: call.participants.clone(),
    // Only a call that rang while the caller was in the conversation can have been missed.
    is_missed: call.was_ringing && call.was_member_at_start && !did_join && !is_starter,
  }
}
