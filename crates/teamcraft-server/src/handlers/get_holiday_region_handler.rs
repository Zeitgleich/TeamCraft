use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, HolidayRegionResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::holiday_regions;
use crate::state::AppState;

/// Returns whose public holidays the caller's series skip.
#[utoipa::path(get, path = "/api/v1/me/holiday-region", tag = "meetings", summary = "Returns whose public holidays the caller's series skip.", responses((status = 200, body = HolidayRegionResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_holiday_region_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<HolidayRegionResponse>, ApiError> {
  let response = holiday_regions::get_holiday_region(&state, &caller).await?;
  Ok(Json(response))
}
