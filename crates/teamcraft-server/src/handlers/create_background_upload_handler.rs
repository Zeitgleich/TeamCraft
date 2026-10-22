use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateBackgroundUploadRequest, CreateBackgroundUploadResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::backgrounds;
use crate::state::AppState;

/// Announces the upload of one of the caller's own (encrypted) call backgrounds.
#[utoipa::path(post, path = "/api/v1/me/backgrounds", tag = "profiles", summary = "Announces the upload of one of the caller's own (encrypted) call backgrounds.", request_body = CreateBackgroundUploadRequest, responses((status = 201, body = CreateBackgroundUploadResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_background_upload_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateBackgroundUploadRequest>,
) -> Result<(StatusCode, Json<CreateBackgroundUploadResponse>), ApiError> {
  let response = backgrounds::create_background_upload(&state, &caller, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
