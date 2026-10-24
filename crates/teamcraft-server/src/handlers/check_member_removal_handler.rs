use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::members;
use crate::state::AppState;

/// Checks that the caller may remove a member (or leave), without removing anybody.
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/members/{user_id}/removal", tag = "conversations", summary = "Checks that the caller may remove a member (or leave), without removing anybody.", description = "Clients ask before they commit the MLS removal, then remove the member with `DELETE`. `204` also when the user is no longer a member.", params(("conversation_id" = ConversationId, Path), ("user_id" = UserId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn check_member_removal_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((conversation_id, user_id)): ApiPath<(ConversationId, UserId)>,
) -> Result<StatusCode, ApiError> {
  members::check_removal(&state, &caller, conversation_id, user_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
