use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AttachmentId, AttachmentResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::attachments;
use crate::state::AppState;

/// Confirms the upload after checking the stored size.
#[utoipa::path(post, path = "/api/v1/attachments/{attachment_id}/complete", tag = "attachments", summary = "Confirms the upload after checking the stored size.", params(("attachment_id" = AttachmentId, Path)), responses((status = 200, body = AttachmentResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn complete_attachment_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(attachment_id): ApiPath<AttachmentId>,
) -> Result<Json<AttachmentResponse>, ApiError> {
  let response = attachments::complete_attachment(&state, &caller, attachment_id).await?;
  Ok(Json(response))
}
