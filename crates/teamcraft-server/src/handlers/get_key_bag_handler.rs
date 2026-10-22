use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, KeyBagResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::key_bag;
use crate::state::AppState;

/// Returns the caller's encrypted key bag.
#[utoipa::path(get, path = "/api/v1/me/key-bag", tag = "e2ee", summary = "Returns the caller's encrypted key bag.", responses((status = 200, body = KeyBagResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_key_bag_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<KeyBagResponse>, ApiError> {
  let response = key_bag::get_key_bag(&state, &caller).await?;
  Ok(Json(response))
}
