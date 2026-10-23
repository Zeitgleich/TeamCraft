use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminBrandsResponse, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::brands;
use crate::state::AppState;

/// Lists every brand (configuration, database, built-in) and the host names brands may use.
#[utoipa::path(get, path = "/api/v1/admin/brands", tag = "admin", summary = "Lists the brands with their source and the served host names.", responses((status = 200, body = AdminBrandsResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_list_brands_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<AdminBrandsResponse>, ApiError> {
  let response = brands::list_brands(&state, &caller).await?;
  Ok(Json(response))
}
