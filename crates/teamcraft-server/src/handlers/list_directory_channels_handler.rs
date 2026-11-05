use axum::Json;
use axum::extract::State;
use teamcraft_proto::{DirectoryChannelsQuery, DirectoryChannelsResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::directory;
use crate::state::AppState;

/// Lists one page of channels across the caller's spaces.
#[utoipa::path(get, path = "/api/v1/directory/channels", tag = "directory", summary = "Lists one page of channels across the caller's spaces.", params(DirectoryChannelsQuery), responses((status = 200, body = DirectoryChannelsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_directory_channels_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiQuery(query): ApiQuery<DirectoryChannelsQuery>,
) -> Result<Json<DirectoryChannelsResponse>, ApiError> {
  let response = directory::list_channels(&state, &caller, &query).await?;
  Ok(Json(response))
}
