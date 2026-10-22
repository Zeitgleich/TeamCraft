use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SpaceId, SpaceMemberResponse, UpdateSpaceMemberRequest, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::spaces;
use crate::state::AppState;

/// Changes a member's space role.
#[utoipa::path(patch, path = "/api/v1/spaces/{space_id}/members/{user_id}", tag = "spaces", summary = "Changes a member's space role.", request_body = UpdateSpaceMemberRequest, params(("space_id" = SpaceId, Path), ("user_id" = UserId, Path)), responses((status = 200, body = SpaceMemberResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn update_space_member_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((space_id, user_id)): ApiPath<(SpaceId, UserId)>,
  ApiJson(request): ApiJson<UpdateSpaceMemberRequest>,
) -> Result<Json<SpaceMemberResponse>, ApiError> {
  let response = spaces::update_space_member(&state, &caller, space_id, user_id, request).await?;
  Ok(Json(response))
}
