use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminNotetakerResponse, ErrorBody, PutNotetakerKeyRequest};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::notetaker;
use crate::state::AppState;

/// Stores, replaces or removes the notetaker's API key (write-only, sealed with the settings key).
#[utoipa::path(put, path = "/api/v1/admin/notetaker/key", tag = "admin", summary = "Stores, replaces or removes the notetaker's OpenRouter key.", request_body = PutNotetakerKeyRequest, responses((status = 200, body = AdminNotetakerResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_put_notetaker_key_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiJson(request): ApiJson<PutNotetakerKeyRequest>,
) -> Result<Json<AdminNotetakerResponse>, ApiError> {
  let response = notetaker::put_key(&state, &caller, request).await?;
  Ok(Json(response))
}
