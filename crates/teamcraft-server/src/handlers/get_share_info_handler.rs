use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ShareInfoResponse};

use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::shares;
use crate::state::AppState;

/// Public: what the download page needs to know about a link.
#[utoipa::path(get, path = "/api/v1/shares/{share_id}", tag = "shares", summary = "Public: what the download page needs to know about a link.", params(("share_id" = String, Path)), responses((status = 200, body = ShareInfoResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_share_info_handler(
  State(state): State<AppState>,
  ApiPath(share_id): ApiPath<String>,
) -> Result<Json<ShareInfoResponse>, ApiError> {
  let response = shares::share_info(&state, &share_id).await?;
  Ok(Json(response))
}
