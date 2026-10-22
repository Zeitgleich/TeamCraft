use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListMeetingsQuery, ListMeetingsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::meetings;
use crate::state::AppState;

/// Lists the caller's meetings in a time range.
#[utoipa::path(get, path = "/api/v1/meetings", tag = "meetings", summary = "Lists the caller's meetings in a time range.", params(ListMeetingsQuery), responses((status = 200, body = ListMeetingsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_meetings_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiQuery(query): ApiQuery<ListMeetingsQuery>,
) -> Result<Json<ListMeetingsResponse>, ApiError> {
  let response = meetings::list_meetings(&state, &caller, query).await?;
  Ok(Json(response))
}
