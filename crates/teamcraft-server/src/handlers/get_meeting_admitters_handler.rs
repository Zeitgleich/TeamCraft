use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, MeetingAdmittersResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::meeting_admitters;
use crate::state::AppState;

/// The organizer-signed list of who may let guests into the meeting chat (its members).
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/meeting-admitters", tag = "meetings", summary = "The organizer-signed list of who may let guests in (members of the meeting chat).", params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = MeetingAdmittersResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_meeting_admitters_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<Json<MeetingAdmittersResponse>, ApiError> {
  let response = meeting_admitters::admitters(&state, &caller, conversation_id).await?;
  Ok(Json(response))
}
