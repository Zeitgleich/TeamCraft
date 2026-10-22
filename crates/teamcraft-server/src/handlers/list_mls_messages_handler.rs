use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, ListMlsMessagesQuery, ListMlsMessagesResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiPath, ApiQuery};
use crate::services::mls;
use crate::state::AppState;

/// Returns the MLS backlog since the last processed entry.
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/mls", tag = "e2ee", summary = "Returns the MLS backlog since the last processed entry.", params(("conversation_id" = ConversationId, Path), ListMlsMessagesQuery), responses((status = 200, body = ListMlsMessagesResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_mls_messages_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiQuery(query): ApiQuery<ListMlsMessagesQuery>,
) -> Result<Json<ListMlsMessagesResponse>, ApiError> {
  let response = mls::list_messages(&state, &caller, conversation_id, query).await?;
  Ok(Json(response))
}
