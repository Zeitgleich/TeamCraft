use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, IdentityKeyResponse, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::identity;
use crate::state::AppState;

/// Returns a user's public identity key.
#[utoipa::path(get, path = "/api/v1/users/{user_id}/identity-key", tag = "e2ee", summary = "Returns a user's public identity key.", params(("user_id" = UserId, Path)), responses((status = 200, body = IdentityKeyResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_identity_key_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(user_id): ApiPath<UserId>,
) -> Result<Json<IdentityKeyResponse>, ApiError> {
  let response = identity::get_identity_key(&state, &caller, user_id).await?;
  Ok(Json(response))
}
