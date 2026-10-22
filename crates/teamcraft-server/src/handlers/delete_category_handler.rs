use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CategoryId, ErrorBody, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::spaces;
use crate::state::AppState;

/// Deletes a category; its channels become uncategorized.
#[utoipa::path(delete, path = "/api/v1/spaces/{space_id}/categories/{category_id}", tag = "spaces", summary = "Deletes a category; its channels become uncategorized.", params(("space_id" = SpaceId, Path), ("category_id" = CategoryId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn delete_category_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((space_id, category_id)): ApiPath<(SpaceId, CategoryId)>,
) -> Result<StatusCode, ApiError> {
  spaces::delete_category(&state, &caller, space_id, category_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
