use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateInviteRequest, ErrorBody, InviteResponse, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::brand::ResolvedBrand;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::invites;
use crate::state::AppState;

/// Invites a person by email as member or guest.
#[utoipa::path(post, path = "/api/v1/spaces/{space_id}/invites", tag = "invites", summary = "Invites a person by email as member or guest.", request_body = CreateInviteRequest, params(("space_id" = SpaceId, Path)), responses((status = 201, body = InviteResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_invite_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  brand: ResolvedBrand,
  ApiPath(space_id): ApiPath<SpaceId>,
  ApiJson(request): ApiJson<CreateInviteRequest>,
) -> Result<(StatusCode, Json<InviteResponse>), ApiError> {
  let response = invites::create_invite(&state, &caller, &brand, space_id, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
