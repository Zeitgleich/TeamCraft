use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, ScimClientId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::scim;
use crate::state::AppState;

/// Revokes a SCIM client: its token stops working and its groups go; the accounts it made stay.
#[utoipa::path(delete, path = "/api/v1/admin/scim/clients/{client_id}", tag = "admin", summary = "Revokes a SCIM client.", params(("client_id" = ScimClientId, Path)), responses((status = 204), (status = 404, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_delete_scim_client_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(client_id): ApiPath<ScimClientId>,
) -> Result<StatusCode, ApiError> {
  scim::delete_client(&state, &caller, client_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
