use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, PutIdentityKeyAttestationRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::identity;
use crate::state::AppState;

/// Stores the identity provider's attestation of the caller's current identity key (e2ee.md §3.1).
#[utoipa::path(put, path = "/api/v1/me/identity-key/attestation", tag = "e2ee", summary = "Stores the identity provider's attestation of the caller's current identity key.", request_body = PutIdentityKeyAttestationRequest, responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_identity_key_attestation_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<PutIdentityKeyAttestationRequest>,
) -> Result<StatusCode, ApiError> {
  identity::put_identity_key_attestation(&state, &caller, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
