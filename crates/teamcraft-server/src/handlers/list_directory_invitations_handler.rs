use axum::Json;
use axum::extract::State;
use teamcraft_proto::{DirectoryInvitationsQuery, DirectoryInvitationsResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::invites;
use crate::state::AppState;

/// Lists one page of the open invitations the caller may manage.
#[utoipa::path(get, path = "/api/v1/directory/invitations", tag = "directory", summary = "Lists one page of the open invitations the caller may manage.", params(DirectoryInvitationsQuery), responses((status = 200, body = DirectoryInvitationsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_directory_invitations_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiQuery(query): ApiQuery<DirectoryInvitationsQuery>,
) -> Result<Json<DirectoryInvitationsResponse>, ApiError> {
  let response = invites::list_directory_invitations(&state, &caller, &query).await?;
  Ok(Json(response))
}
