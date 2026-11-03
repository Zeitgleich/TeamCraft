use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateAttachmentRequest, CreateMultipartAttachmentResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::attachments;
use crate::state::AppState;

/// Announces a large upload that goes up in parts.
#[utoipa::path(post, path = "/api/v1/attachments/multipart", tag = "attachments", summary = "Announces a large upload that goes up in parts.", request_body = CreateAttachmentRequest, responses((status = 201, body = CreateMultipartAttachmentResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_multipart_attachment_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateAttachmentRequest>,
) -> Result<(StatusCode, Json<CreateMultipartAttachmentResponse>), ApiError> {
  let response = attachments::create_multipart_attachment(&state, &caller, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
