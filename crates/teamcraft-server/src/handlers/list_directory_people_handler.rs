use axum::Json;
use axum::extract::State;
use teamcraft_proto::{DirectoryPeopleQuery, DirectoryPeopleResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::directory;
use crate::state::AppState;

/// Lists one page of the people directory.
#[utoipa::path(get, path = "/api/v1/directory/people", tag = "directory", summary = "Lists one page of the people directory.", params(DirectoryPeopleQuery), responses((status = 200, body = DirectoryPeopleResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_directory_people_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiQuery(query): ApiQuery<DirectoryPeopleQuery>,
) -> Result<Json<DirectoryPeopleResponse>, ApiError> {
  let response = directory::list_people(&state, &caller, &query).await?;
  Ok(Json(response))
}
