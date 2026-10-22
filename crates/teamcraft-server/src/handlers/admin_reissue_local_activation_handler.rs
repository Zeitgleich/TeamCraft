use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, LocalGuestResponse, UserId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::local_accounts;
use crate::state::AppState;

/// Locks a local account and mails a new activation link.
#[utoipa::path(post, path = "/api/v1/admin/local-accounts/{user_id}/activation", tag = "admin", summary = "Locks a local account and mails a new activation link.", params(("user_id" = UserId, Path)), responses((status = 200, body = LocalGuestResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_reissue_local_activation_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(user_id): ApiPath<UserId>,
) -> Result<Json<LocalGuestResponse>, ApiError> {
  let response = local_accounts::reissue_activation(&state, &caller, user_id).await?;
  Ok(Json(response))
}
