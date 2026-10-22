use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, GetMeResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::convert::presented_name;
use crate::state::AppState;

/// Returns the authenticated user.
#[utoipa::path(
  get,
  path = "/api/v1/me",
  tag = "me",
  summary = "Returns the authenticated user.",
  responses((status = 200, body = GetMeResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)),
  security(("bearer" = []))
)]
pub async fn get_me_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<GetMeResponse>, ApiError> {
  Ok(Json(GetMeResponse {
    id: caller.user.id,
    display_name: presented_name(&state.policy.get().directory, &caller.user),
    maybe_username: caller.user.maybe_username,
    role: caller.role,
  }))
}
