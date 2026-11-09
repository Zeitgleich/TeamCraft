use axum::Json;
use axum::extract::State;
use teamcraft_proto::{BridgeAddressResponse, ConversationId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::runtime_settings;
use crate::state::AppState;

/// Returns where producers reach the integration bridge from outside.
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/bridge-address", tag = "conversations", summary = "Returns the bridge's public address for a channel's sources.", description = "The admin setting \"bridge public URL\", for people who may configure the channel's integrations. Holds that one value and nothing else; absent when none is set.", params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = BridgeAddressResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_bridge_address_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<Json<BridgeAddressResponse>, ApiError> {
  let response = runtime_settings::get_bridge_address(&state, &caller, conversation_id).await?;
  Ok(Json(response))
}
