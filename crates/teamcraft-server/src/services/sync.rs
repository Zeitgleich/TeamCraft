//! `GET /sync/heads` (issue #714): where each of the caller's conversations and spaces stands, so a
//! client coming back after the resume window reads only what changed. Three queries, whatever the
//! size of the conversations' histories and member lists.

use std::collections::HashMap;

use teamcraft_crypto::change_digest::change_digest;
use teamcraft_db::SpaceListing;
use teamcraft_proto::{ConversationHead, SpaceHead, SyncHeadsResponse};

use super::access::is_guest;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Stands for a member list that has no token yet: unchanged since migration 0075.
const NO_TOKEN: &str = "-";

pub async fn sync_heads(state: &AppState, caller: &AuthenticatedUser) -> Result<SyncHeadsResponse, ApiError> {
  let is_guest = is_guest(caller);
  let directory = directory_fingerprint(state)?;
  let rows = state
    .database
    .list_conversation_heads(caller.user.id, is_guest)
    .await?;
  let conversations = rows
    .into_iter()
    .map(|row| ConversationHead {
      conversation_id: row.conversation,
      maybe_last_message_id: row.maybe_last_message_id,
      maybe_last_mls_message_id: row.maybe_last_mls_message_id,
      members_digest: members_digest(row.maybe_members_token.as_deref(), &directory),
    })
    .collect();
  Ok(SyncHeadsResponse {
    conversations,
    spaces: space_heads(state, caller, is_guest, &directory).await?,
  })
}

/// Guests have no space member lists (`GET /spaces/{id}/members` refuses them), so no heads either.
async fn space_heads(
  state: &AppState,
  caller: &AuthenticatedUser,
  is_guest: bool,
  directory: &str,
) -> Result<Vec<SpaceHead>, ApiError> {
  if is_guest {
    return Ok(Vec::new());
  }
  let spaces: Vec<_> = state
    .database
    .list_spaces_for_user(caller.user.id, SpaceListing::Mine)
    .await?
    .into_iter()
    .filter(|view| view.maybe_role.is_some())
    .map(|view| view.space.id)
    .collect();
  let tokens: HashMap<_, _> = state
    .database
    .list_space_member_tokens(&spaces)
    .await?
    .into_iter()
    .collect();
  Ok(
    spaces
      .into_iter()
      .map(|space| SpaceHead {
        space_id: space,
        members_digest: members_digest(tokens.get(&space).map(String::as_str), directory),
      })
      .collect(),
  )
}

/// A member list answer is its rows (the token changes with them) as the directory policy presents
/// them (the names' format).
fn members_digest(maybe_token: Option<&str>, directory: &str) -> String {
  let token = maybe_token.unwrap_or(NO_TOKEN);
  change_digest(format!("{token}/{directory}").as_bytes())
}

fn directory_fingerprint(state: &AppState) -> Result<String, ApiError> {
  serde_json::to_string(&state.policy.get().directory).map_err(|error| ApiError::Internal(error.to_string()))
}
