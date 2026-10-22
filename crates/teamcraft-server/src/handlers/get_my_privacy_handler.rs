use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, PrivacyResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::presence;
use crate::state::AppState;

/// Returns the privacy choices of the caller.
#[utoipa::path(get, path = "/api/v1/me/privacy", tag = "presence", summary = "Returns the privacy choices of the caller.", responses((status = 200, body = PrivacyResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_my_privacy_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<PrivacyResponse>, ApiError> {
  let response = presence::get_my_privacy(&state, &caller).await?;
  Ok(Json(response))
}
