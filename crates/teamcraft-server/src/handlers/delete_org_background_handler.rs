use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, OrgBackgroundId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::org_backgrounds;
use crate::state::AppState;

/// Admins: deletes an organization background the workspace policy no longer lists.
#[utoipa::path(delete, path = "/api/v1/admin/backgrounds/{background_id}", tag = "admin", summary = "Admins: deletes an organization background the workspace policy no longer lists.", params(("background_id" = OrgBackgroundId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn delete_org_background_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(background_id): ApiPath<OrgBackgroundId>,
) -> Result<StatusCode, ApiError> {
  org_backgrounds::delete(&state, &caller, background_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
