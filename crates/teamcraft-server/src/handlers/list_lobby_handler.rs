use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListLobbyResponse, MeetingId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::meeting_guests;
use crate::state::AppState;

/// Lists the guests waiting in the lobby (host only).
#[utoipa::path(get, path = "/api/v1/meetings/{meeting_id}/lobby", tag = "meetings", summary = "Lists the guests waiting in the lobby (host only).", params(("meeting_id" = MeetingId, Path)), responses((status = 200, body = ListLobbyResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_lobby_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
) -> Result<Json<ListLobbyResponse>, ApiError> {
  let response = meeting_guests::list_lobby(&state, &caller, meeting_id).await?;
  Ok(Json(response))
}
