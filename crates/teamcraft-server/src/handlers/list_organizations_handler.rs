use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListOrganizationsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::directory;
use crate::state::AppState;

/// Lists the organizations people belong to (for the directory filter).
#[utoipa::path(get, path = "/api/v1/organizations", tag = "directory", summary = "Lists the organizations people belong to.", responses((status = 200, body = ListOrganizationsResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_organizations_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListOrganizationsResponse>, ApiError> {
  let response = directory::list_organizations(&state, &caller).await?;
  Ok(Json(response))
}
