use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ChangeWebPushSubscriptionRequest, ErrorBody};

use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::devices;
use crate::state::AppState;

/// Public: replaces a device's rotated Web Push subscription, authenticated by its push key.
#[utoipa::path(post, path = "/api/v1/push/web/subscription-change", tag = "devices", summary = "Public: replaces a device's rotated Web Push subscription, authenticated by its push key.", request_body = ChangeWebPushSubscriptionRequest, responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 409, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn change_web_push_subscription_handler(
  State(state): State<AppState>,
  ApiJson(request): ApiJson<ChangeWebPushSubscriptionRequest>,
) -> Result<StatusCode, ApiError> {
  devices::change_web_push_subscription(&state, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
