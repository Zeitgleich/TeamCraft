use axum::extract::State;

use crate::services::scim::{self, ScimCaller, ScimResponse};
use crate::state::AppState;

/// `GET /scim/v2/ServiceProviderConfig`: what the endpoint supports.
pub async fn scim_service_provider_config_handler(
  State(state): State<AppState>,
  _caller: ScimCaller,
) -> ScimResponse {
  scim::service_provider_config(&state)
}
