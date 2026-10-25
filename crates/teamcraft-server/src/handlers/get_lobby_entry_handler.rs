use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, LobbyEntry, MeetingId, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::meeting_guests;
use crate::state::AppState;

/// Returns one guest's lobby entry with the proof they gave (host only).
#[utoipa::path(get, path = "/api/v1/meetings/{meeting_id}/lobby/{user_id}", tag = "meetings", summary = "Returns one guest's lobby entry with the proof they gave (host only).", params(("meeting_id" = MeetingId, Path), ("user_id" = UserId, Path)), responses((status = 200, body = LobbyEntry), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_lobby_entry_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((meeting_id, user_id)): ApiPath<(MeetingId, UserId)>,
) -> Result<Json<LobbyEntry>, ApiError> {
  let entry = meeting_guests::lobby_entry(&state, &caller, meeting_id, user_id).await?;
  Ok(Json(entry))
}
