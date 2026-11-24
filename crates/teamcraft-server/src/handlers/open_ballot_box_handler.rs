use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, MessageId, OpenBallotBoxRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::poll_ballots;
use crate::state::AppState;

/// Opens the ballot box of an anonymous poll; only its sender may. (e2ee.md §4.12.1)
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/messages/{message_id}/ballot-box", tag = "messages", summary = "Opens the ballot box of an anonymous poll; only its sender may.", params(("conversation_id" = ConversationId, Path), ("message_id" = MessageId, Path)), request_body = OpenBallotBoxRequest, responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn open_ballot_box_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((conversation_id, message_id)): ApiPath<(ConversationId, MessageId)>,
  Json(request): Json<OpenBallotBoxRequest>,
) -> Result<StatusCode, ApiError> {
  poll_ballots::open_box(
    &state,
    &caller,
    conversation_id,
    message_id,
    request.maybe_closes_at_ms,
  )
  .await?;
  Ok(StatusCode::NO_CONTENT)
}
