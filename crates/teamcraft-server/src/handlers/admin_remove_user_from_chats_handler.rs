use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, UserId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::admin;
use crate::state::AppState;

/// Removes a deactivated user from all chats: their devices are removed and they leave every
/// conversation and space; members' clients rotate the keys. A reactivation does not undo it.
#[utoipa::path(post, path = "/api/v1/admin/users/{user_id}/remove-from-chats", tag = "admin", summary = "Removes a deactivated user from all chats.", params(("user_id" = UserId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "The user is not deactivated")), security(("bearer" = [])))]
pub async fn admin_remove_user_from_chats_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(user_id): ApiPath<UserId>,
) -> Result<StatusCode, ApiError> {
  admin::remove_from_chats_as_admin(&state, &caller, user_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
