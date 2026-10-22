use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, OrgBackgroundId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::org_backgrounds;
use crate::state::AppState;

/// Admins: confirms an organization background upload after checking the stored size.
#[utoipa::path(post, path = "/api/v1/admin/backgrounds/{background_id}/complete", tag = "admin", summary = "Admins: confirms an organization background upload after checking the stored size.", params(("background_id" = OrgBackgroundId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn complete_org_background_upload_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(background_id): ApiPath<OrgBackgroundId>,
) -> Result<StatusCode, ApiError> {
  org_backgrounds::complete_upload(&state, &caller, background_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
