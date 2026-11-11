use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, MeetingHostsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::meeting_hosts;
use crate::state::AppState;

/// The organizer-signed host lists of the meeting chat: every handover and the newest (its members).
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/meeting-hosts", tag = "meetings", summary = "The organizer-signed host lists: every handover and the newest (members of the meeting chat).", params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = MeetingHostsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_meeting_hosts_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<Json<MeetingHostsResponse>, ApiError> {
  let response = meeting_hosts::hosts(&state, &caller, conversation_id).await?;
  Ok(Json(response))
}
