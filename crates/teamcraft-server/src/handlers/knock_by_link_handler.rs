use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, KnockByLinkResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::meeting_guests;
use crate::state::AppState;

/// A member opened a meeting link: the meeting it names, and whether they are in or wait in the
/// lobby.
#[utoipa::path(post, path = "/api/v1/meetings/link/{token}/knock", tag = "meetings", summary = "A member opened a meeting link: the meeting, and whether they are in or wait in the lobby.", params(("token" = String, Path, description = "The token from the meeting link's path")), responses((status = 200, body = KnockByLinkResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn knock_by_link_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(token): ApiPath<String>,
) -> Result<Json<KnockByLinkResponse>, ApiError> {
  let response = meeting_guests::knock_by_link(&state, &caller, &token).await?;
  Ok(Json(response))
}
