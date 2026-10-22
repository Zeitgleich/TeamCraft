use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SpaceId, SpaceNotificationSettings};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::notifications;
use crate::state::AppState;

/// Returns the caller's notification level and mute time for a whole space.
#[utoipa::path(get, path = "/api/v1/spaces/{space_id}/notification-settings", tag = "notifications", summary = "Returns the caller's notification level and mute time for a whole space.", params(("space_id" = SpaceId, Path)), responses((status = 200, body = SpaceNotificationSettings), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_space_notification_settings_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
) -> Result<Json<SpaceNotificationSettings>, ApiError> {
  let response = notifications::get_space_settings(&state, &caller, space_id).await?;
  Ok(Json(response))
}
