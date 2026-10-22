use axum::Json;
use teamcraft_proto::{ErrorBody, GetMeResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;

/// Returns the authenticated user.
#[utoipa::path(
  get,
  path = "/api/v1/me",
  tag = "me",
  summary = "Returns the authenticated user.",
  responses((status = 200, body = GetMeResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)),
  security(("bearer" = []))
)]
pub async fn get_me_handler(caller: AuthenticatedUser) -> Result<Json<GetMeResponse>, ApiError> {
  Ok(Json(GetMeResponse {
    id: caller.user.id,
    maybe_username: caller.user.maybe_username,
    display_name: caller.user.display_name,
    role: caller.role,
  }))
}
