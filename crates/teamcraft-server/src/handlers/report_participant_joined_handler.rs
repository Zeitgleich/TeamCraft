use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, SfuParticipantReport};

use crate::error::ApiError;
use crate::extract::{ApiJson, BearerToken};
use crate::services::calls;
use crate::state::AppState;

/// Internal: the SFU reports that a device joined a call.
#[utoipa::path(post, path = "/api/internal/v1/calls/participants/joined", tag = "internal", summary = "Internal: the SFU reports that a device joined a call.", request_body = SfuParticipantReport, responses((status = 204), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn report_participant_joined_handler(
  State(state): State<AppState>,
  BearerToken(maybe_token): BearerToken,
  ApiJson(report): ApiJson<SfuParticipantReport>,
) -> Result<StatusCode, ApiError> {
  calls::authenticate_sfu(&state, maybe_token.as_deref())?;
  calls::report_joined(&state, report).await?;
  Ok(StatusCode::NO_CONTENT)
}
