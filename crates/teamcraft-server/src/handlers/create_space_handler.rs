use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateSpaceRequest, ErrorBody, SpaceResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::spaces;
use crate::state::AppState;

/// Creates a space (the caller becomes its space admin).
#[utoipa::path(post, path = "/api/v1/spaces", tag = "spaces", summary = "Creates a space (the caller becomes its space admin).", request_body = CreateSpaceRequest, responses((status = 201, body = SpaceResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_space_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateSpaceRequest>,
) -> Result<(StatusCode, Json<SpaceResponse>), ApiError> {
  let response = spaces::create_space(&state, &caller, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
