use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{AdminJoinRequestResponse, ConversationId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::admin_join_requests;
use crate::state::AppState;

/// A workspace admin asks the owners of a private channel to let them in.
#[utoipa::path(post, path = "/api/v1/conversations/{conversation_id}/admin-join-requests", tag = "conversations", summary = "A workspace admin asks the owners of a private channel to let them in.", description = "Only while the signed policy says `admin_self_join = owner_approval` (admin.md §2.4). Idempotent: an open request of the caller is answered as it is, and nobody is told again. The channel's owners approve by adding the admin (their MLS commit), or decline.", params(("conversation_id" = ConversationId, Path)), responses((status = 201, body = AdminJoinRequestResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn request_admin_join_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<(StatusCode, Json<AdminJoinRequestResponse>), ApiError> {
  let response = admin_join_requests::request(&state, &caller, conversation_id).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
