use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, PrivacyResponse, UpdatePrivacyRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::presence;
use crate::state::AppState;

/// Sets who may see when the caller was last active.
#[utoipa::path(put, path = "/api/v1/me/privacy", tag = "presence", summary = "Sets who may see when the caller was last active.", request_body = UpdatePrivacyRequest, responses((status = 200, body = PrivacyResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_my_privacy_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<UpdatePrivacyRequest>,
) -> Result<Json<PrivacyResponse>, ApiError> {
  let response = presence::put_my_privacy(&state, &caller, request).await?;
  Ok(Json(response))
}
