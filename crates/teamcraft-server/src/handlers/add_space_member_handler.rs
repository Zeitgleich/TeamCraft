use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{AddSpaceMemberRequest, ErrorBody, SpaceId, SpaceMemberResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::spaces;
use crate::state::AppState;

/// Adds a person to a space (guests need invite_guest).
#[utoipa::path(post, path = "/api/v1/spaces/{space_id}/members", tag = "spaces", summary = "Adds a person to a space (guests need invite_guest).", request_body = AddSpaceMemberRequest, params(("space_id" = SpaceId, Path)), responses((status = 201, body = SpaceMemberResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn add_space_member_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
  ApiJson(request): ApiJson<AddSpaceMemberRequest>,
) -> Result<(StatusCode, Json<SpaceMemberResponse>), ApiError> {
  let response = spaces::add_space_member(&state, &caller, space_id, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
