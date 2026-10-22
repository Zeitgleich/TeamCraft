use axum::Json;
use axum::extract::State;
use teamcraft_proto::{DeviceId, DeviceResponse, ErrorBody, SetDeviceCertificateRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::devices;
use crate::state::AppState;

/// Replaces the device's cross-signature with the final certificate.
#[utoipa::path(put, path = "/api/v1/devices/{device_id}/certificate", tag = "devices", summary = "Replaces the device's cross-signature with the final certificate.", request_body = SetDeviceCertificateRequest, params(("device_id" = DeviceId, Path)), responses((status = 200, body = DeviceResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn set_device_certificate_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(device_id): ApiPath<DeviceId>,
  ApiJson(request): ApiJson<SetDeviceCertificateRequest>,
) -> Result<Json<DeviceResponse>, ApiError> {
  let response = devices::set_device_certificate(&state, &caller, device_id, request).await?;
  Ok(Json(response))
}
