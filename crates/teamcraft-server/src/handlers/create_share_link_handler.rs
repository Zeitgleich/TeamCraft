use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateShareLinkRequest, ErrorBody, ShareLinkResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::shares;
use crate::state::AppState;

/// Makes a share link to a file.
#[utoipa::path(post, path = "/api/v1/shares", tag = "shares", summary = "Makes a share link to a file.", request_body = CreateShareLinkRequest, responses((status = 201, body = ShareLinkResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_share_link_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateShareLinkRequest>,
) -> Result<(StatusCode, Json<ShareLinkResponse>), ApiError> {
  let response = shares::create_share(&state, &caller, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
