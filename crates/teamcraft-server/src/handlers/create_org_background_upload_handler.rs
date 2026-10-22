use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateOrgBackgroundUploadRequest, CreateOrgBackgroundUploadResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::org_backgrounds;
use crate::state::AppState;

/// Admins: announces the upload of an organization call background (a JPEG, stored unencrypted).
#[utoipa::path(post, path = "/api/v1/admin/backgrounds", tag = "admin", summary = "Admins: announces the upload of an organization call background (a JPEG, stored unencrypted).", request_body = CreateOrgBackgroundUploadRequest, responses((status = 201, body = CreateOrgBackgroundUploadResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_org_background_upload_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateOrgBackgroundUploadRequest>,
) -> Result<(StatusCode, Json<CreateOrgBackgroundUploadResponse>), ApiError> {
  let response = org_backgrounds::create_upload(&state, &caller, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
