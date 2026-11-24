use axum::Json;
use axum::extract::State;
use teamcraft_proto::{BallotBoxResponse, ConversationId, ErrorBody, MessageId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::poll_ballots;
use crate::state::AppState;

/// Closes an anonymous poll's ballot box: its sender, or who may delete others' posts. (e2ee.md §4.12.1)
#[utoipa::path(post, path = "/api/v1/conversations/{conversation_id}/messages/{message_id}/ballot-box/close", tag = "messages", summary = "Closes an anonymous poll's ballot box: its sender, or who may delete others' posts.", params(("conversation_id" = ConversationId, Path), ("message_id" = MessageId, Path)), responses((status = 200, body = BallotBoxResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn close_ballot_box_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((conversation_id, message_id)): ApiPath<(ConversationId, MessageId)>,
) -> Result<Json<BallotBoxResponse>, ApiError> {
  Ok(Json(
    poll_ballots::close_box(&state, &caller, conversation_id, message_id).await?,
  ))
}
