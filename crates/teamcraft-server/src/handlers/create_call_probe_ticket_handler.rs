use axum::Json;
use axum::extract::State;
use teamcraft_proto::{CallProbeTicketResponse, CreateCallProbeTicketRequest, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::call_probe;
use crate::state::AppState;

/// Issues a short-lived ticket for measuring the caller's link to the SFU before joining (#1959).
#[utoipa::path(post, path = "/api/v1/calls/probe-ticket", tag = "calls", summary = "Issues a short-lived ticket for measuring the caller's link to the SFU before joining.", request_body = CreateCallProbeTicketRequest, responses((status = 200, body = CallProbeTicketResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_call_probe_ticket_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateCallProbeTicketRequest>,
) -> Result<Json<CallProbeTicketResponse>, ApiError> {
  let response = call_probe::create_probe_ticket(&state, &caller, request).await?;
  Ok(Json(response))
}
