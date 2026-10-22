use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, NotificationSettings};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::notifications;
use crate::state::AppState;

/// Replaces the caller's notification preferences (DND schedule, push mode, receipts, thread follow).
#[utoipa::path(put, path = "/api/v1/me/notification-settings", tag = "notifications", summary = "Replaces the caller's notification preferences (DND schedule, push mode, receipts, thread follow).", request_body = NotificationSettings, responses((status = 200, body = NotificationSettings), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_notification_settings_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<NotificationSettings>,
) -> Result<Json<NotificationSettings>, ApiError> {
  let response = notifications::put_settings(&state, &caller, request).await?;
  Ok(Json(response))
}
