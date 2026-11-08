use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, HolidayRegionResponse, SetHolidayRegionRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::holiday_regions;
use crate::state::AppState;

/// Sets the caller's own holiday region, or the instance's again.
#[utoipa::path(put, path = "/api/v1/me/holiday-region", tag = "meetings", summary = "Sets the caller's own holiday region, or the instance's again.", request_body = SetHolidayRegionRequest, responses((status = 200, body = HolidayRegionResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_holiday_region_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<SetHolidayRegionRequest>,
) -> Result<Json<HolidayRegionResponse>, ApiError> {
  let response = holiday_regions::put_holiday_region(&state, &caller, request).await?;
  Ok(Json(response))
}
