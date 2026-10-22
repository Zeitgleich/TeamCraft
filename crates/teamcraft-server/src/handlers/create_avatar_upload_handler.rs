use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateAvatarUploadRequest, CreateAvatarUploadResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::profiles;
use crate::state::AppState;

/// Announces an avatar upload and returns a presigned upload URL.
#[utoipa::path(post, path = "/api/v1/me/avatar", tag = "profiles", summary = "Announces an avatar upload and returns a presigned upload URL.", request_body = CreateAvatarUploadRequest, responses((status = 201, body = CreateAvatarUploadResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_avatar_upload_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateAvatarUploadRequest>,
) -> Result<(StatusCode, Json<CreateAvatarUploadResponse>), ApiError> {
  let response = profiles::create_avatar_upload(&state, &caller, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
