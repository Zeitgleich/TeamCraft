use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, MlsGroupInfoResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::mls;
use crate::state::AppState;

/// The MLS group information for the current epoch, for a member's new device to join by external commit.
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/mls/group-info", tag = "e2ee", summary = "The MLS group information for the current epoch, for a member's new device to join by external commit.", params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = MlsGroupInfoResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_mls_group_info_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<Json<MlsGroupInfoResponse>, ApiError> {
  Ok(Json(mls::group_info(&state, &caller, conversation_id).await?))
}
