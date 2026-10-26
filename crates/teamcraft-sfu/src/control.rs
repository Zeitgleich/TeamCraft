//! Meeting host controls from `teamcraft-server` (mute all, remove, lock, end): an internal HTTP
//! route next to `/healthz`, authenticated with the SFU registration token. The server decides
//! who may do what; the SFU only carries it out.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use subtle::ConstantTimeEq;
use teamcraft_proto::{ErrorBody, ErrorCode, SfuControlAction, SfuControlRequest, SfuRosterReport};

use crate::room::RoomError;
use crate::server::SfuControl;

/// Route of the control endpoint.
pub const CONTROL_PATH: &str = "/internal/v1/calls/control";
/// Route a starting server reads the roster from, to take over the calls that kept running.
pub const ROSTER_PATH: &str = "/internal/v1/calls/roster";

#[derive(Clone)]
struct ControlState {
  control: SfuControl,
  registration_token: Arc<str>,
}

/// The control route; merge it into the internal listener's router.
pub fn control_router(control: SfuControl, registration_token: impl Into<Arc<str>>) -> Router {
  Router::new()
    .route(CONTROL_PATH, post(control_handler))
    .route(ROSTER_PATH, get(roster_handler))
    .with_state(ControlState {
      control,
      registration_token: registration_token.into(),
    })
}

async fn control_handler(
  State(state): State<ControlState>,
  headers: HeaderMap,
  Json(request): Json<SfuControlRequest>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
  if !is_authorized(&headers, &state.registration_token) {
    return Err(error(
      StatusCode::UNAUTHORIZED,
      ErrorCode::Unauthorized,
      "invalid registration token",
    ));
  }
  if request.action == SfuControlAction::Unknown {
    return Err(error(
      StatusCode::BAD_REQUEST,
      ErrorCode::InvalidRequest,
      "unknown action",
    ));
  }
  match state.control.apply(&request) {
    Ok(()) => Ok(StatusCode::NO_CONTENT),
    Err(RoomError::CallNotFound(call_id)) => Err(error(
      StatusCode::NOT_FOUND,
      ErrorCode::CallNotFound,
      &format!("call {call_id} is not on this SFU"),
    )),
    Err(other) => Err(error(
      StatusCode::INTERNAL_SERVER_ERROR,
      ErrorCode::Internal,
      &other.to_string(),
    )),
  }
}

async fn roster_handler(
  State(state): State<ControlState>,
  headers: HeaderMap,
) -> Result<Json<SfuRosterReport>, (StatusCode, Json<ErrorBody>)> {
  if !is_authorized(&headers, &state.registration_token) {
    return Err(error(
      StatusCode::UNAUTHORIZED,
      ErrorCode::Unauthorized,
      "invalid registration token",
    ));
  }
  Ok(Json(state.control.roster()))
}

/// `Authorization: Bearer <token>`, compared in constant time.
fn is_authorized(headers: &HeaderMap, token: &str) -> bool {
  headers
    .get(header::AUTHORIZATION)
    .and_then(|value| value.to_str().ok())
    .and_then(|value| value.strip_prefix("Bearer "))
    .is_some_and(|presented| bool::from(presented.as_bytes().ct_eq(token.as_bytes())))
}

fn error(status: StatusCode, code: ErrorCode, message: &str) -> (StatusCode, Json<ErrorBody>) {
  (status, Json(ErrorBody::new(code, message)))
}
