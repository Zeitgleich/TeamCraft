use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, KeyBagResponse, PutKeyBagRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::key_bag;
use crate::state::AppState;

/// Stores the encrypted key bag (compare-and-swap on the version).
#[utoipa::path(put, path = "/api/v1/me/key-bag", tag = "e2ee", summary = "Stores the encrypted key bag (compare-and-swap on the version).", request_body = PutKeyBagRequest, responses((status = 200, body = KeyBagResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_key_bag_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<PutKeyBagRequest>,
) -> Result<Json<KeyBagResponse>, ApiError> {
  let response = key_bag::put_key_bag(&state, &caller, request).await?;
  Ok(Json(response))
}
