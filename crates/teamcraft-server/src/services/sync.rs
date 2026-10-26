//! `GET /sync/heads` (issue #714): where each of the caller's conversations and spaces stands, so a
//! client coming back after the resume window reads only what changed. Three queries plus one per
//! space, whatever the size of the conversations' histories.

use std::collections::HashMap;

use teamcraft_crypto::change_digest::change_digest;
use teamcraft_db::{ConversationMember, SpaceListing};
use teamcraft_proto::{ConversationHead, ConversationId, SpaceHead, SyncHeadsResponse};

use super::access::is_guest;
use super::members::members_response;
use super::spaces::space_members_response;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

pub async fn sync_heads(state: &AppState, caller: &AuthenticatedUser) -> Result<SyncHeadsResponse, ApiError> {
  let is_guest = is_guest(caller);
  let rows = state
    .database
    .list_conversation_heads(caller.user.id, is_guest)
    .await?;
  let mut members: HashMap<ConversationId, Vec<ConversationMember>> = HashMap::new();
  for (conversation, member) in state
    .database
    .list_members_of_conversations_of(caller.user.id)
    .await?
  {
    members.entry(conversation).or_default().push(member);
  }
  let conversations = rows
    .into_iter()
    .map(|row| {
      let list = members.remove(&row.conversation).unwrap_or_default();
      Ok(ConversationHead {
        conversation_id: row.conversation,
        maybe_last_message_id: row.maybe_last_message_id,
        maybe_last_mls_message_id: row.maybe_last_mls_message_id,
        members_digest: digest(&members_response(state, &list))?,
      })
    })
    .collect::<Result<Vec<_>, ApiError>>()?;
  Ok(SyncHeadsResponse {
    conversations,
    spaces: space_heads(state, caller, is_guest).await?,
  })
}

/// Guests have no space member lists (`GET /spaces/{id}/members` refuses them), so no heads either.
async fn space_heads(
  state: &AppState,
  caller: &AuthenticatedUser,
  is_guest: bool,
) -> Result<Vec<SpaceHead>, ApiError> {
  if is_guest {
    return Ok(Vec::new());
  }
  let views = state
    .database
    .list_spaces_for_user(caller.user.id, SpaceListing::Mine)
    .await?;
  let mut heads = Vec::new();
  for view in views.iter().filter(|view| view.maybe_role.is_some()) {
    let members = state.database.list_space_members(view.space.id).await?;
    heads.push(SpaceHead {
      space_id: view.space.id,
      members_digest: digest(&space_members_response(state, &members))?,
    });
  }
  Ok(heads)
}

/// The digest of an answer as the client would receive it.
fn digest(answer: &impl serde::Serialize) -> Result<String, ApiError> {
  let bytes = serde_json::to_vec(answer).map_err(|error| ApiError::Internal(error.to_string()))?;
  Ok(change_digest(&bytes))
}
