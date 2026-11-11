use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, PutMeetingHostsRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::meeting_hosts;
use crate::state::AppState;

/// Publishes the organizer-signed list of who hosts the meeting, or hands it over (the meeting's
/// organizer only); `409` when a newer one is stored.
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/meeting-hosts", tag = "meetings", summary = "Publishes the organizer-signed host list; one naming another organizer hands the meeting over (organizer only).", params(("conversation_id" = ConversationId, Path)), request_body = PutMeetingHostsRequest, responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_meeting_hosts_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<PutMeetingHostsRequest>,
) -> Result<StatusCode, ApiError> {
  meeting_hosts::put_hosts(&state, &caller, conversation_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
