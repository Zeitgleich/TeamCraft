use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, UserId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::admin;
use crate::state::AppState;

/// Deletes a user (admin.md §3.2.1): devices, key material, sessions and memberships go, direct
/// messages with them are deleted, the identity provider's identity is released; the account stays
/// as a "Deleted user" tombstone on old messages. Idempotent.
#[utoipa::path(delete, path = "/api/v1/admin/users/{user_id}", tag = "admin", summary = "Deletes a user; the account stays as a tombstone on old messages.", params(("user_id" = UserId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 400, body = ErrorBody, description = "`last_owner`"), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`cannot_delete_self` or `last_admin`")), security(("bearer" = [])))]
pub async fn admin_delete_user_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(user_id): ApiPath<UserId>,
) -> Result<StatusCode, ApiError> {
  admin::delete_user_as_admin(&state, &caller, user_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
