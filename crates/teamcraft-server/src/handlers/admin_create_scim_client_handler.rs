use axum::Json;
use axum::extract::State;
use teamcraft_proto::{CreateScimClientRequest, ErrorBody, ScimClientTokenResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::scim;
use crate::state::AppState;

/// Makes a SCIM client; the answer carries its token, the only time it is shown.
#[utoipa::path(post, path = "/api/v1/admin/scim/clients", tag = "admin", summary = "Makes a SCIM client.", request_body = CreateScimClientRequest, responses((status = 200, body = ScimClientTokenResponse), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody, description = "The instance has the most SCIM clients it keeps"), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_create_scim_client_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiJson(request): ApiJson<CreateScimClientRequest>,
) -> Result<Json<ScimClientTokenResponse>, ApiError> {
  Ok(Json(scim::create_client(&state, &caller, request).await?))
}
