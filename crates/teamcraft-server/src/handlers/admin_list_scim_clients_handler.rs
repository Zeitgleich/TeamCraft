use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ScimClientsResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::scim;
use crate::state::AppState;

/// The SCIM clients (docs/provisioning.md §7); never their tokens.
#[utoipa::path(get, path = "/api/v1/admin/scim/clients", tag = "admin", summary = "Lists the SCIM clients.", responses((status = 200, body = ScimClientsResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_list_scim_clients_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<ScimClientsResponse>, ApiError> {
  Ok(Json(scim::list_clients(&state, &caller).await?))
}
