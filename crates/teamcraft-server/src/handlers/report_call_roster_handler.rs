use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, SfuRosterReport};

use crate::auth::SfuCaller;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::calls;
use crate::state::AppState;

/// Internal: the SFU reports every call it runs and who is in it; the server's roster follows.
#[utoipa::path(post, path = "/api/internal/v1/calls/roster", tag = "internal", summary = "Internal: the SFU reports every call it runs and who is in it; the server's roster follows.", request_body = SfuRosterReport, responses((status = 204), (status = 401, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn report_call_roster_handler(
  State(state): State<AppState>,
  _caller: SfuCaller,
  ApiJson(report): ApiJson<SfuRosterReport>,
) -> Result<StatusCode, ApiError> {
  calls::report_roster(&state, report).await?;
  Ok(StatusCode::NO_CONTENT)
}
