use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, SpaceId, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::spaces;
use crate::state::AppState;

/// Removes a member from a space and its channels (or leaves).
#[utoipa::path(delete, path = "/api/v1/spaces/{space_id}/members/{user_id}", tag = "spaces", summary = "Removes a member from a space and its channels (or leaves).", params(("space_id" = SpaceId, Path), ("user_id" = UserId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn remove_space_member_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((space_id, user_id)): ApiPath<(SpaceId, UserId)>,
) -> Result<StatusCode, ApiError> {
  spaces::remove_space_member(&state, &caller, space_id, user_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
