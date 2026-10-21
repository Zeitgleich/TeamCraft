use axum::Json;
use teamcraft_proto::GetMeResponse;

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;

/// `GET /api/v1/me`
pub async fn get_me_handler(caller: AuthenticatedUser) -> Result<Json<GetMeResponse>, ApiError> {
  Ok(Json(GetMeResponse {
    id: caller.user.id,
    maybe_username: caller.user.maybe_username,
    display_name: caller.user.display_name,
    role: caller.role,
  }))
}
