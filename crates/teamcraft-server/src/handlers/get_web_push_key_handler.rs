use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, WebPushKeyResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Returns the VAPID public key browsers subscribe to Web Push with.
#[utoipa::path(
  get,
  path = "/api/v1/push/web-key",
  tag = "devices",
  summary = "Returns the VAPID public key browsers subscribe to Web Push with.",
  responses((status = 200, body = WebPushKeyResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)),
  security(("bearer" = []))
)]
pub async fn get_web_push_key_handler(
  State(state): State<AppState>,
  _caller: AuthenticatedUser,
) -> Result<Json<WebPushKeyResponse>, ApiError> {
  Ok(Json(WebPushKeyResponse {
    maybe_public_key: state.push.web_public_key().map(str::to_owned),
  }))
}
