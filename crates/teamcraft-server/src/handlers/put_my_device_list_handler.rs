use axum::Json;
use axum::extract::State;
use teamcraft_proto::{DeviceListResponse, ErrorBody, PutDeviceListRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::device_lists;
use crate::state::AppState;

/// Replaces the caller's signed device list (compare-and-swap on the sequence).
#[utoipa::path(put, path = "/api/v1/me/device-list", tag = "e2ee", summary = "Replaces the caller's signed device list (compare-and-swap on the sequence).", request_body = PutDeviceListRequest, responses((status = 200, body = DeviceListResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_my_device_list_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<PutDeviceListRequest>,
) -> Result<Json<DeviceListResponse>, ApiError> {
  let response = device_lists::put_device_list(&state, &caller, request).await?;
  Ok(Json(response))
}
