use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::ErrorBody;

use crate::error::ApiError;
use crate::services::crash_reports;
use crate::state::AppState;

/// Relays one crash report envelope, scrubbed on the device, to the instance's crash reports
/// backend (docs/crash-reports.md). No sign-in: an app that crashed before or during sign-in
/// reports too. Off: 404 `crash_reports_off`, and the app drops its queue.
#[utoipa::path(post, path = "/api/v1/crash-reports", tag = "health", summary = "Relays a crash report to the instance's crash reports backend.", request_body(content = String, content_type = "application/x-sentry-envelope"), responses((status = 202, description = "Forwarded"), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 413, body = ErrorBody), (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn post_crash_report_handler(
  State(state): State<AppState>,
  body: Bytes,
) -> Result<StatusCode, ApiError> {
  crash_reports::relay(&state, body).await?;
  Ok(StatusCode::ACCEPTED)
}
