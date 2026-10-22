use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, LobbyStateResponse, MeetingId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::meeting_guests;
use crate::state::AppState;

/// Returns the caller's own place in the lobby.
#[utoipa::path(get, path = "/api/v1/meetings/{meeting_id}/lobby/me", tag = "meetings", summary = "Returns the caller's own place in the lobby.", params(("meeting_id" = MeetingId, Path)), responses((status = 200, body = LobbyStateResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_my_lobby_state_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
) -> Result<Json<LobbyStateResponse>, ApiError> {
  let response = meeting_guests::my_lobby_state(&state, &caller, meeting_id).await?;
  Ok(Json(response))
}
