use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminNotetakerResponse, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::notetaker;
use crate::state::AppState;

/// Returns whether the notetaker's API key is stored; the key itself never comes back.
#[utoipa::path(get, path = "/api/v1/admin/notetaker", tag = "admin", summary = "Returns whether the notetaker's OpenRouter key is stored.", responses((status = 200, body = AdminNotetakerResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_get_notetaker_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<AdminNotetakerResponse>, ApiError> {
  let response = notetaker::get_notetaker(&state, &caller).await?;
  Ok(Json(response))
}
