use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationResponse, CreateDmRequest, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::conversations;
use crate::state::AppState;

/// Opens a DM (returns the existing one if there is one).
#[utoipa::path(post, path = "/api/v1/dms", tag = "conversations", summary = "Opens a DM (returns the existing one if there is one).", request_body = CreateDmRequest, responses((status = 201, body = ConversationResponse), (status = 200, body = ConversationResponse, description = "Already existed"), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_dm_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateDmRequest>,
) -> Result<(StatusCode, Json<ConversationResponse>), ApiError> {
  let created = conversations::create_dm(&state, &caller, request).await?;
  let status = if created.is_new {
    StatusCode::CREATED
  } else {
    StatusCode::OK
  };
  Ok((status, Json(created.value)))
}
