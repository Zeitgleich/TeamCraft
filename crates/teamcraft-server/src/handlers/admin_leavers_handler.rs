use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use teamcraft_proto::{ClaimLeaverRequest, ErrorBody, ListLeaversQuery, ListLeaversResponse, UserId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::leavers;
use crate::state::AppState;

/// Who the identity provider deactivated and is still in conversations, spaces or holds devices.
#[utoipa::path(get, path = "/api/v1/admin/leavers", tag = "admin", summary = "Leavers still in encryption groups.", params(ListLeaversQuery), responses((status = 200, body = ListLeaversResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_list_leavers_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  Query(query): Query<ListLeaversQuery>,
) -> Result<Json<ListLeaversResponse>, ApiError> {
  Ok(Json(leavers::list(&state, &caller, query).await?))
}

/// Claims a leaver for the caller's device before it signs their removals.
#[utoipa::path(post, path = "/api/v1/admin/leavers/{user_id}/claim", tag = "admin", summary = "Claims a leaver before signing their removals.", params(("user_id" = UserId, Path)), request_body = ClaimLeaverRequest, responses((status = 204, description = "Claimed by this device"), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody, description = "No admin, or not the caller's device"), (status = 404, body = ErrorBody, description = "Not a leaver (any more)"), (status = 409, body = ErrorBody, description = "Another device holds a claim (`leaver_claimed`)")), security(("bearer" = [])))]
pub async fn admin_claim_leaver_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  Path(user): Path<UserId>,
  Json(request): Json<ClaimLeaverRequest>,
) -> Result<StatusCode, ApiError> {
  leavers::claim(&state, &caller, user, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
