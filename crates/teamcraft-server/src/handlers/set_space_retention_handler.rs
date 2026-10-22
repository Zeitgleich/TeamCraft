use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, RetentionStatusResponse, SetRetentionRequest, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::retention;
use crate::state::AppState;

/// Sets a space's retention within the workspace limit.
#[utoipa::path(put, path = "/api/v1/spaces/{space_id}/retention", tag = "retention", summary = "Sets a space's retention within the workspace limit.", request_body = SetRetentionRequest, params(("space_id" = SpaceId, Path)), responses((status = 200, body = RetentionStatusResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn set_space_retention_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
  ApiJson(request): ApiJson<SetRetentionRequest>,
) -> Result<Json<RetentionStatusResponse>, ApiError> {
  let response = retention::set_space_retention(&state, &caller, space_id, request).await?;
  Ok(Json(response))
}
