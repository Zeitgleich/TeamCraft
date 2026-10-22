use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, NotificationSettings};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::notifications;
use crate::state::AppState;

/// Returns the caller's notification preferences.
#[utoipa::path(get, path = "/api/v1/me/notification-settings", tag = "notifications", summary = "Returns the caller's notification preferences.", responses((status = 200, body = NotificationSettings), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_notification_settings_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<NotificationSettings>, ApiError> {
  let response = notifications::get_settings(&state, &caller).await?;
  Ok(Json(response))
}
