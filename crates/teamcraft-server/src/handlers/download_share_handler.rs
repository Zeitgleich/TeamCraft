use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ShareDownloadResponse};

use crate::brand::ResolvedBrand;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::shares;
use crate::state::AppState;

/// Public: counts a download and returns the ciphertext's URL.
#[utoipa::path(post, path = "/api/v1/shares/{share_id}/download", tag = "shares", summary = "Public: counts a download and returns the ciphertext's URL.", params(("share_id" = String, Path)), responses((status = 200, body = ShareDownloadResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn download_share_handler(
  State(state): State<AppState>,
  brand: ResolvedBrand,
  ApiPath(share_id): ApiPath<String>,
) -> Result<Json<ShareDownloadResponse>, ApiError> {
  let response = shares::download_share(&state, &brand, &share_id).await?;
  Ok(Json(response))
}
