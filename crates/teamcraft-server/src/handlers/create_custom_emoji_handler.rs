use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateCustomEmojiRequest, CustomEmojiResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::custom_emoji;
use crate::state::AppState;

/// their device signed.
#[utoipa::path(post, path = "/api/v1/custom-emoji", tag = "custom emoji", summary = "Admins: adds a custom emoji with the document their device signed.", request_body = CreateCustomEmojiRequest, responses((status = 201, body = CustomEmojiResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_custom_emoji_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateCustomEmojiRequest>,
) -> Result<(StatusCode, Json<CustomEmojiResponse>), ApiError> {
  let created = custom_emoji::create(&state, &caller, request).await?;
  Ok((StatusCode::CREATED, Json(created)))
}
