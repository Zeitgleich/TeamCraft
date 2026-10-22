use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateAttachmentRequest, CreateAttachmentResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::attachments;
use crate::state::AppState;

/// Announces an upload and returns a presigned upload URL.
#[utoipa::path(post, path = "/api/v1/attachments", tag = "attachments", summary = "Announces an upload and returns a presigned upload URL.", request_body = CreateAttachmentRequest, responses((status = 201, body = CreateAttachmentResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_attachment_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateAttachmentRequest>,
) -> Result<(StatusCode, Json<CreateAttachmentResponse>), ApiError> {
  let response = attachments::create_attachment(&state, &caller, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
