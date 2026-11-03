use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ShareDownloadRequest, ShareDownloadResponse};

use crate::brand::ResolvedBrand;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::shares;
use crate::state::AppState;

/// Public: counts a download and returns the ciphertext's URL.
#[utoipa::path(post, path = "/api/v1/shares/{share_id}/download", tag = "shares", summary = "Public: counts a download and returns the ciphertext's URL.", params(("share_id" = String, Path)), request_body(content = ShareDownloadRequest, description = "Optional: the grant of an earlier answer, to retry without counting."), responses((status = 200, body = ShareDownloadResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn download_share_handler(
  State(state): State<AppState>,
  brand: ResolvedBrand,
  ApiPath(share_id): ApiPath<String>,
  maybe_body: Option<Json<ShareDownloadRequest>>,
) -> Result<Json<ShareDownloadResponse>, ApiError> {
  let maybe_grant = maybe_body.and_then(|Json(body)| body.maybe_grant);
  let response = shares::download_share(&state, &brand, &share_id, maybe_grant.as_deref()).await?;
  Ok(Json(response))
}
