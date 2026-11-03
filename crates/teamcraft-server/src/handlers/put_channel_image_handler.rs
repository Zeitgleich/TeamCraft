use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use teamcraft_proto::{ConversationId, ConversationResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::channel_images;
use crate::state::AppState;

/// Sets the picture of a channel (people who manage the channel). The body is a PNG, JPEG or WebP file whose
/// `Content-Type` matches it; the server re-encodes it as PNG. SVG is refused.
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/image", tag = "conversations", summary = "Sets the picture of a channel.", params(("conversation_id" = ConversationId, Path)), request_body(content = Vec<u8>, content_type = "image/*"), responses((status = 200, body = ConversationResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 413, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_channel_image_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  headers: HeaderMap,
  body: Bytes,
) -> Result<Json<ConversationResponse>, ApiError> {
  let declared_type = headers
    .get(header::CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .unwrap_or_default();
  let response =
    channel_images::put_channel_image(&state, &caller, conversation_id, declared_type, body).await?;
  Ok(Json(response))
}
