use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::spaces;
use crate::state::AppState;

/// Deletes a space with all of its channels for everybody.
#[utoipa::path(delete, path = "/api/v1/spaces/{space_id}", tag = "spaces", summary = "Deletes a space with all of its channels for everybody.", description = "Archived or not; needs `delete_space`. Every channel goes as `DELETE /conversations/{id}` does (e2ee.md §4.6). `404` once it is gone.", params(("space_id" = SpaceId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn delete_space_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
) -> Result<StatusCode, ApiError> {
  spaces::delete_space(&state, &caller, space_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
