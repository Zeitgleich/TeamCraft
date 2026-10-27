use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ConversationMemberResponse, ErrorBody, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::admin_join_requests;
use crate::state::AppState;

/// An owner of the channel lets the admin in (the server membership; the owner's client commits the MLS add).
#[utoipa::path(post, path = "/api/v1/conversations/{conversation_id}/admin-join-requests/{user_id}/approve", tag = "conversations", summary = "An owner of the channel lets the admin in.", description = "The server membership only: the owner's client commits the MLS add and posts the encrypted \"joined as a workspace admin\" message next (admin.md §2.4). Answers the membership also when the admin is in already, so a retry goes on to the commit; `404 admin_join_request_not_found` when the request was declined, withdrawn or expired.", params(("conversation_id" = ConversationId, Path), ("user_id" = UserId, Path)), responses((status = 200, body = ConversationMemberResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn approve_admin_join_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((conversation_id, user_id)): ApiPath<(ConversationId, UserId)>,
) -> Result<Json<ConversationMemberResponse>, ApiError> {
  Ok(Json(
    admin_join_requests::approve(&state, &caller, conversation_id, user_id).await?,
  ))
}
