use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ScimClientId, ScimClientTokenResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::scim;
use crate::state::AppState;

/// A new token for a SCIM client, shown this once; the old one stops working at once.
#[utoipa::path(post, path = "/api/v1/admin/scim/clients/{client_id}/token", tag = "admin", summary = "Rotates a SCIM client's token.", params(("client_id" = ScimClientId, Path)), responses((status = 200, body = ScimClientTokenResponse), (status = 404, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_rotate_scim_client_token_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(client_id): ApiPath<ScimClientId>,
) -> Result<Json<ScimClientTokenResponse>, ApiError> {
  Ok(Json(scim::rotate_token(&state, &caller, client_id).await?))
}
