use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ProvisionUsersRequest, ProvisionUsersResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::import;
use crate::state::AppState;

/// Creates accounts from the identity provider before their first sign-in (import account only).
#[utoipa::path(post, path = "/api/v1/admin/import/users", tag = "admin", summary = "Creates accounts from the identity provider before their first sign-in (import account only).", request_body = ProvisionUsersRequest, responses((status = 200, body = ProvisionUsersResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_import_provision_users_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiJson(request): ApiJson<ProvisionUsersRequest>,
) -> Result<Json<ProvisionUsersResponse>, ApiError> {
  Ok(Json(import::provision_users(&state, &caller, request).await?))
}
