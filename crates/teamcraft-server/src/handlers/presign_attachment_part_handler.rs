use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AttachmentId, AttachmentPartUrlResponse, ErrorBody, PresignAttachmentPartRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::attachments;
use crate::state::AppState;

/// Returns a presigned upload URL for one part of an upload in parts.
#[utoipa::path(post, path = "/api/v1/attachments/{attachment_id}/parts", tag = "attachments", summary = "Returns a presigned upload URL for one part of an upload in parts.", params(("attachment_id" = AttachmentId, Path)), request_body = PresignAttachmentPartRequest, responses((status = 200, body = AttachmentPartUrlResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn presign_attachment_part_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(attachment_id): ApiPath<AttachmentId>,
  ApiJson(request): ApiJson<PresignAttachmentPartRequest>,
) -> Result<Json<AttachmentPartUrlResponse>, ApiError> {
  let response = attachments::presign_part(&state, &caller, attachment_id, request).await?;
  Ok(Json(response))
}
