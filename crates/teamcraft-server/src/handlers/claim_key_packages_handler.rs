use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ClaimKeyPackagesRequest, ClaimKeyPackagesResponse, ErrorBody, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::key_packages;
use crate::state::AppState;

/// Takes one key package per device of a user, to add them to a conversation.
#[utoipa::path(post, path = "/api/v1/users/{user_id}/key-packages/claim", tag = "e2ee", summary = "Takes one key package per device of a user, to add them to a conversation.", request_body = ClaimKeyPackagesRequest, params(("user_id" = UserId, Path)), responses((status = 200, body = ClaimKeyPackagesResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn claim_key_packages_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(user_id): ApiPath<UserId>,
  ApiJson(request): ApiJson<ClaimKeyPackagesRequest>,
) -> Result<Json<ClaimKeyPackagesResponse>, ApiError> {
  let response = key_packages::claim_key_packages(&state, &caller, user_id, request).await?;
  Ok(Json(response))
}
