use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, ListSystemEventsQuery, ListSystemEventsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiPath, ApiQuery};
use crate::services::system_events;
use crate::state::AppState;

/// Lists what happened to a conversation (members added, archived, ...).
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/system-events", tag = "conversations", summary = "Lists what happened to a conversation (members added, archived, ...).", params(("conversation_id" = ConversationId, Path), ListSystemEventsQuery), responses((status = 200, body = ListSystemEventsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_system_events_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiQuery(query): ApiQuery<ListSystemEventsQuery>,
) -> Result<Json<ListSystemEventsResponse>, ApiError> {
  let response = system_events::list_system_events(&state, &caller, conversation_id, query).await?;
  Ok(Json(response))
}
