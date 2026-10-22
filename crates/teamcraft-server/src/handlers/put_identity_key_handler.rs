use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, IdentityKeyResponse, UploadIdentityKeyRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::identity;
use crate::state::AppState;

/// Publishes the caller's public identity key and its OIDC attestation.
#[utoipa::path(put, path = "/api/v1/me/identity-key", tag = "e2ee", summary = "Publishes the caller's public identity key and its OIDC attestation.", request_body = UploadIdentityKeyRequest, responses((status = 200, body = IdentityKeyResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_identity_key_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<UploadIdentityKeyRequest>,
) -> Result<Json<IdentityKeyResponse>, ApiError> {
  let response = identity::put_identity_key(&state, &caller, request).await?;
  Ok(Json(response))
}
