use axum::Json;
use axum::extract::State;
use teamcraft_proto::{BallotBoxResponse, CastBallotRequest, ConversationId, ErrorBody, MessageId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::poll_ballots;
use crate::state::AppState;

/// Keeps the caller's sealed ballot, replacing an earlier one, while the poll is open. (e2ee.md §4.12.1)
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/messages/{message_id}/ballot-box/ballot", tag = "messages", summary = "Keeps the caller's sealed ballot, replacing an earlier one, while the poll is open.", params(("conversation_id" = ConversationId, Path), ("message_id" = MessageId, Path)), request_body = CastBallotRequest, responses((status = 200, body = BallotBoxResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn cast_ballot_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((conversation_id, message_id)): ApiPath<(ConversationId, MessageId)>,
  Json(request): Json<CastBallotRequest>,
) -> Result<Json<BallotBoxResponse>, ApiError> {
  let response =
    poll_ballots::set_ballot(&state, &caller, conversation_id, message_id, Some(request.ballot)).await?;
  Ok(Json(response))
}
