use axum::Json;
use axum::extract::State;
use teamcraft_proto::{DeviceId, ErrorBody, KeyPackageStatusResponse, UploadKeyPackagesRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::key_packages;
use crate::state::AppState;

/// Uploads MLS key packages and the last-resort package of a device.
#[utoipa::path(post, path = "/api/v1/devices/{device_id}/key-packages", tag = "e2ee", summary = "Uploads MLS key packages and the last-resort package of a device.", request_body = UploadKeyPackagesRequest, params(("device_id" = DeviceId, Path)), responses((status = 200, body = KeyPackageStatusResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn upload_key_packages_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(device_id): ApiPath<DeviceId>,
  ApiJson(request): ApiJson<UploadKeyPackagesRequest>,
) -> Result<Json<KeyPackageStatusResponse>, ApiError> {
  let response = key_packages::upload_key_packages(&state, &caller, device_id, request).await?;
  Ok(Json(response))
}
