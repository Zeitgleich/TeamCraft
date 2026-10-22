use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListPresenceQuery, ListPresenceResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::presence;
use crate::state::AppState;

/// Returns the presence of some users.
#[utoipa::path(get, path = "/api/v1/presence", tag = "presence", summary = "Returns the presence of some users.", params(ListPresenceQuery), responses((status = 200, body = ListPresenceResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_presence_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiQuery(query): ApiQuery<ListPresenceQuery>,
) -> Result<Json<ListPresenceResponse>, ApiError> {
  let response = presence::list_presence(&state, &caller, &query.user_ids).await?;
  Ok(Json(response))
}
