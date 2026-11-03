use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AttachmentId, AttachmentResponse, CompleteMultipartAttachmentRequest, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::attachments;
use crate::state::AppState;

/// Joins the uploaded parts and confirms the upload after checking the stored size.
#[utoipa::path(post, path = "/api/v1/attachments/{attachment_id}/multipart/complete", tag = "attachments", summary = "Joins the uploaded parts and confirms the upload after checking the stored size.", params(("attachment_id" = AttachmentId, Path)), request_body = CompleteMultipartAttachmentRequest, responses((status = 200, body = AttachmentResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn complete_multipart_attachment_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(attachment_id): ApiPath<AttachmentId>,
  ApiJson(request): ApiJson<CompleteMultipartAttachmentRequest>,
) -> Result<Json<AttachmentResponse>, ApiError> {
  let response = attachments::complete_multipart_attachment(&state, &caller, attachment_id, request).await?;
  Ok(Json(response))
}
