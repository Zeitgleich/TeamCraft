use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AttachmentDownloadResponse, AttachmentId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::attachments;
use crate::state::AppState;

/// Returns a presigned download URL.
#[utoipa::path(get, path = "/api/v1/attachments/{attachment_id}/download", tag = "attachments", summary = "Returns a presigned download URL.", params(("attachment_id" = AttachmentId, Path)), responses((status = 200, body = AttachmentDownloadResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn download_attachment_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(attachment_id): ApiPath<AttachmentId>,
) -> Result<Json<AttachmentDownloadResponse>, ApiError> {
  let response = attachments::download_attachment(&state, &caller, attachment_id).await?;
  Ok(Json(response))
}
