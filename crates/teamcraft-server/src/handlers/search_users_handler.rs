use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SearchUsersQuery, SearchUsersResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::directory;
use crate::state::AppState;

/// Searches the directory (guests: only people they share a conversation with).
#[utoipa::path(get, path = "/api/v1/users", tag = "directory", summary = "Searches the directory (guests: only people they share a conversation with).", params(SearchUsersQuery), responses((status = 200, body = SearchUsersResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn search_users_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiQuery(query): ApiQuery<SearchUsersQuery>,
) -> Result<Json<SearchUsersResponse>, ApiError> {
  let response = directory::search_users(&state, &caller, &query.query).await?;
  Ok(Json(response))
}
