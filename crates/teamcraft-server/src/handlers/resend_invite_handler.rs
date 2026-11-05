use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, InviteId, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::invites;
use crate::state::AppState;

/// Sends a pending invite's e-mail again.
#[utoipa::path(post, path = "/api/v1/spaces/{space_id}/invites/{invite_id}/resend", tag = "invites", summary = "Sends a pending invite's e-mail again.", params(("space_id" = SpaceId, Path), ("invite_id" = InviteId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn resend_invite_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((space_id, invite_id)): ApiPath<(SpaceId, InviteId)>,
) -> Result<StatusCode, ApiError> {
  invites::resend_invite(&state, &caller, space_id, invite_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
