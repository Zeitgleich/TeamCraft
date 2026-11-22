use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, SfuRecordingReport};

use crate::auth::SfuCaller;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::call_recordings;
use crate::state::AppState;

/// Internal: the SFU reports that a device's recording started or stopped (the audit log, #2117).
#[utoipa::path(post, path = "/api/internal/v1/calls/recording", tag = "internal", summary = "Internal: the SFU reports that a device's recording started or stopped.", request_body = SfuRecordingReport, responses((status = 204), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn report_call_recording_handler(
  State(state): State<AppState>,
  _caller: SfuCaller,
  ApiJson(report): ApiJson<SfuRecordingReport>,
) -> Result<StatusCode, ApiError> {
  call_recordings::report(&state, report).await?;
  Ok(StatusCode::NO_CONTENT)
}
