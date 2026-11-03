use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListAdminJoinRequestsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::admin_join_requests;
use crate::state::AppState;

/// The open admin join requests of channels the caller owns, and the caller's own.
#[utoipa::path(get, path = "/api/v1/admin-join-requests", tag = "conversations", summary = "The open admin join requests of channels the caller owns, and the caller's own.", description = "Oldest first. Requests that can't be acted on (archived or public channel, the admin is in already or deactivated) are left out.", responses((status = 200, body = ListAdminJoinRequestsResponse), (status = 401, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_admin_join_requests_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListAdminJoinRequestsResponse>, ApiError> {
  Ok(Json(admin_join_requests::list(&state, &caller).await?))
}
