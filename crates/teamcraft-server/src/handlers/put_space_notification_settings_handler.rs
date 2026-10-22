use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SpaceId, SpaceNotificationSettings};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::notifications;
use crate::state::AppState;

/// Sets the caller's notification level and mute time for a whole space.
#[utoipa::path(put, path = "/api/v1/spaces/{space_id}/notification-settings", tag = "notifications", summary = "Sets the caller's notification level and mute time for a whole space.", request_body = SpaceNotificationSettings, params(("space_id" = SpaceId, Path)), responses((status = 200, body = SpaceNotificationSettings), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_space_notification_settings_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
  ApiJson(request): ApiJson<SpaceNotificationSettings>,
) -> Result<Json<SpaceNotificationSettings>, ApiError> {
  let response = notifications::put_space_settings(&state, &caller, space_id, request).await?;
  Ok(Json(response))
}
