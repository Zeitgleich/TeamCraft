use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{AttachmentId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::attachments;
use crate::state::AppState;

/// Keeps a file of the caller's unsent draft as they send it: it is no longer removed with the draft.
#[utoipa::path(post, path = "/api/v1/attachments/{attachment_id}/keep", tag = "attachments", summary = "Keeps a file of the caller's unsent draft as they send it: it is no longer removed with the draft.", params(("attachment_id" = AttachmentId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn keep_attachment_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(attachment_id): ApiPath<AttachmentId>,
) -> Result<StatusCode, ApiError> {
  attachments::keep_attachment(&state, &caller, attachment_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
