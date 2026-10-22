use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListAdminSpacesResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::admin;
use crate::state::AppState;

/// Lists all spaces with member and channel counts.
#[utoipa::path(get, path = "/api/v1/admin/spaces", tag = "admin", summary = "Lists all spaces with member and channel counts.", responses((status = 200, body = ListAdminSpacesResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_list_spaces_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListAdminSpacesResponse>, ApiError> {
  let response = admin::list_spaces(&state, &caller).await?;
  Ok(Json(response))
}
