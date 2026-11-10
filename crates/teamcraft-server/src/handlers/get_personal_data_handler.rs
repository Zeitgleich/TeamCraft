use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, PersonalDataResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::personal_data;
use crate::state::AppState;

/// Returns everything the server holds about the caller that it can read, for their data export.
#[utoipa::path(
  get,
  path = "/api/v1/me/personal-data",
  tag = "me",
  summary = "Returns everything the server holds about the caller that it can read, for their data export.",
  responses((status = 200, body = PersonalDataResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)),
  security(("bearer" = []))
)]
pub async fn get_personal_data_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<PersonalDataResponse>, ApiError> {
  let response = personal_data::personal_data(&state, &caller).await?;
  Ok(Json(response))
}
