use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ConversationResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::channel_images;
use crate::state::AppState;

/// Removes the picture of a channel (people who manage the channel).
#[utoipa::path(delete, path = "/api/v1/conversations/{conversation_id}/image", tag = "conversations", summary = "Removes the picture of a channel.", params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = ConversationResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn delete_channel_image_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<Json<ConversationResponse>, ApiError> {
  let response = channel_images::delete_channel_image(&state, &caller, conversation_id).await?;
  Ok(Json(response))
}
