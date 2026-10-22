use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListSpacesResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::spaces;
use crate::state::AppState;

/// Lists the spaces the caller can see.
#[utoipa::path(get, path = "/api/v1/spaces", tag = "spaces", summary = "Lists the spaces the caller can see.", responses((status = 200, body = ListSpacesResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_spaces_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListSpacesResponse>, ApiError> {
  let response = spaces::list_spaces(&state, &caller).await?;
  Ok(Json(response))
}
