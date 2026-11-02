use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, RemoveMemberQuery, SpaceId, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiPath, ApiQuery};
use crate::services::spaces;
use crate::state::AppState;

/// Removes a member from a space and its channels (or leaves).
#[utoipa::path(delete, path = "/api/v1/spaces/{space_id}/members/{user_id}", tag = "spaces", summary = "Removes a member from a space and its channels (or leaves).", params(("space_id" = SpaceId, Path), ("user_id" = UserId, Path), ("delete_if_last" = Option<bool>, Query, description = "Leaving: `true` deletes the private channels nobody else is in.")), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn remove_space_member_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((space_id, user_id)): ApiPath<(SpaceId, UserId)>,
  ApiQuery(query): ApiQuery<RemoveMemberQuery>,
) -> Result<StatusCode, ApiError> {
  spaces::remove_space_member(&state, &caller, space_id, user_id, &query).await?;
  Ok(StatusCode::NO_CONTENT)
}
