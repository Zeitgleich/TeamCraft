use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::ErrorBody;

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::shares;
use crate::state::AppState;

/// Revokes a share link.
#[utoipa::path(delete, path = "/api/v1/shares/{share_id}", tag = "shares", summary = "Revokes a share link.", params(("share_id" = String, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn revoke_share_link_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(share_id): ApiPath<String>,
) -> Result<StatusCode, ApiError> {
  shares::revoke_share(&state, &caller, &share_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
