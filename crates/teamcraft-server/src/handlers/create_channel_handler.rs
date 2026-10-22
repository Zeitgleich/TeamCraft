use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationResponse, CreateChannelRequest, ErrorBody, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::conversations;
use crate::state::AppState;

/// Creates a channel in a space.
#[utoipa::path(post, path = "/api/v1/spaces/{space_id}/channels", tag = "conversations", summary = "Creates a channel in a space.", request_body = CreateChannelRequest, params(("space_id" = SpaceId, Path)), responses((status = 201, body = ConversationResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_channel_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
  ApiJson(request): ApiJson<CreateChannelRequest>,
) -> Result<(StatusCode, Json<ConversationResponse>), ApiError> {
  let response = conversations::create_channel(&state, &caller, space_id, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
