use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{BotTokenResponse, CreateBotRequest, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::bots;
use crate::state::AppState;

/// Registers a bot (integration bridge) and returns its token once.
#[utoipa::path(post, path = "/api/v1/admin/bots", tag = "admin", summary = "Registers a bot (integration bridge) and returns its token once.", request_body = CreateBotRequest, responses((status = 201, body = BotTokenResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_create_bot_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateBotRequest>,
) -> Result<(StatusCode, Json<BotTokenResponse>), ApiError> {
  let response = bots::create_bot(&state, &caller, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
