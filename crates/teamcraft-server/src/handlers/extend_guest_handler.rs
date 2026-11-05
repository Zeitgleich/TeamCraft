use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ExtendGuestRequest, SpaceId, SpaceMemberResponse, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::guests;
use crate::state::AppState;

/// Extends a guest's membership of a space: their sponsor up to the policy's limit, admins further.
#[utoipa::path(post, path = "/api/v1/spaces/{space_id}/guests/{user_id}/extend", tag = "spaces", summary = "Extends a guest's membership of a space.", request_body = ExtendGuestRequest, params(("space_id" = SpaceId, Path), ("user_id" = UserId, Path)), responses((status = 200, body = SpaceMemberResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn extend_guest_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((space_id, user_id)): ApiPath<(SpaceId, UserId)>,
  ApiJson(request): ApiJson<ExtendGuestRequest>,
) -> Result<Json<SpaceMemberResponse>, ApiError> {
  let response = guests::extend(&state, &caller, space_id, user_id, request).await?;
  Ok(Json(response))
}
