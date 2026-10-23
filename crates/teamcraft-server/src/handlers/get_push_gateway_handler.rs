use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, PushGatewayBinding, PushGatewayResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Returns how this server reaches its push gateway, for apps that seal their device token for it.
#[utoipa::path(
  get,
  path = "/api/v1/push/gateway",
  tag = "devices",
  summary = "Returns how this server reaches its push gateway, for apps that seal their device token for it.",
  responses((status = 200, body = PushGatewayResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)),
  security(("bearer" = []))
)]
pub async fn get_push_gateway_handler(
  State(state): State<AppState>,
  _caller: AuthenticatedUser,
) -> Result<Json<PushGatewayResponse>, ApiError> {
  Ok(Json(PushGatewayResponse {
    maybe_gateway: state.push.gateway_binding().map(|binding| PushGatewayBinding {
      url: binding.url.clone(),
      server_id: binding.server_id.clone(),
      providers: binding.providers.clone(),
    }),
  }))
}
