use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{AttachmentId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::attachments;
use crate::state::AppState;

/// Gives up an unfinished upload of the caller: aborts its parts or deletes its object.
#[utoipa::path(delete, path = "/api/v1/attachments/{attachment_id}", tag = "attachments", summary = "Gives up an unfinished upload of the caller: aborts its parts or deletes its object.", params(("attachment_id" = AttachmentId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn abandon_attachment_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(attachment_id): ApiPath<AttachmentId>,
) -> Result<StatusCode, ApiError> {
  attachments::abandon_attachment(&state, &caller, attachment_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
