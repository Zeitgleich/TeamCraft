use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, MlsStrictHintResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::mls;
use crate::state::AppState;

/// The group's epoch and the strict hint, for a workspace admin outside the group who signs a strict channel's roles change (e2ee.md §4.6.2).
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/mls/strict", tag = "e2ee", summary = "The group's epoch and the strict hint, for a workspace admin outside the group who signs a strict channel's roles change.", description = "What the members told the server about the channel's strict roles: whether it is strict and the version and hash of the last roles change. A hint, never a decision. Members read it, and holders of `manage_members`.", params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = MlsStrictHintResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_mls_strict_hint_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<Json<MlsStrictHintResponse>, ApiError> {
  Ok(Json(mls::strict_hint(&state, &caller, conversation_id).await?))
}
