use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, PushHealthResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::push_diagnostics;
use crate::state::AppState;

/// Push registration and delivery counts of the caller's own devices since the server started.
#[utoipa::path(get, path = "/api/v1/me/push-health", tag = "devices", summary = "Push registration and delivery counts of the caller's own devices since the server started.", responses((status = 200, body = PushHealthResponse), (status = 401, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_my_push_health_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<PushHealthResponse>, ApiError> {
  let response = push_diagnostics::own_push_health(&state, &caller).await?;
  Ok(Json(response))
}
