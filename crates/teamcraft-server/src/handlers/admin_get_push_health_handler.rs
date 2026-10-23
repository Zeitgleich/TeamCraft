use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, PushHealthResponse, UserId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::push_diagnostics;
use crate::state::AppState;

/// Push delivery counts per device of a user since the server started (counts only).
#[utoipa::path(get, path = "/api/v1/admin/users/{user_id}/push-health", tag = "admin", summary = "Push delivery counts per device of a user since the server started (counts only).", params(("user_id" = UserId, Path)), responses((status = 200, body = PushHealthResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_get_push_health_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(user_id): ApiPath<UserId>,
) -> Result<Json<PushHealthResponse>, ApiError> {
  let response = push_diagnostics::push_health(&state, &caller, user_id).await?;
  Ok(Json(response))
}
