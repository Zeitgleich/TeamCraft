use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, OrgBackgroundDownloadResponse, OrgBackgroundId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::org_backgrounds;
use crate::state::AppState;

/// A download URL for an organization call background (everybody in the workspace).
#[utoipa::path(get, path = "/api/v1/org-backgrounds/{background_id}/download", tag = "profiles", summary = "A download URL for an organization call background (everybody in the workspace).", params(("background_id" = OrgBackgroundId, Path)), responses((status = 200, body = OrgBackgroundDownloadResponse), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn download_org_background_handler(
  State(state): State<AppState>,
  _caller: AuthenticatedUser,
  ApiPath(background_id): ApiPath<OrgBackgroundId>,
) -> Result<Json<OrgBackgroundDownloadResponse>, ApiError> {
  Ok(Json(org_backgrounds::download(&state, background_id).await?))
}
