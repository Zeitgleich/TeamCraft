use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListDevicesResponse, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::identity;
use crate::state::AppState;

/// Lists a user's device keys with their cross-signatures.
#[utoipa::path(get, path = "/api/v1/users/{user_id}/devices", tag = "e2ee", summary = "Lists a user's device keys with their cross-signatures.", params(("user_id" = UserId, Path)), responses((status = 200, body = ListDevicesResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_user_devices_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(user_id): ApiPath<UserId>,
) -> Result<Json<ListDevicesResponse>, ApiError> {
  let response = identity::list_user_devices(&state, &caller, user_id).await?;
  Ok(Json(response))
}
