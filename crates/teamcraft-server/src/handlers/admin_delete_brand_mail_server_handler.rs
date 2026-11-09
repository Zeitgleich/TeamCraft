use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminBrandsResponse, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::brand_mail;
use crate::state::AppState;

/// Removes a brand's own mail server; its mail goes through the instance's again.
#[utoipa::path(delete, path = "/api/v1/admin/brands/{brand_id}/mail-server", tag = "admin", summary = "Removes a brand's own mail server.", params(("brand_id" = String, Path, description = "Brand id")), responses((status = 200, body = AdminBrandsResponse), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_delete_brand_mail_server_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(brand_id): ApiPath<String>,
) -> Result<Json<AdminBrandsResponse>, ApiError> {
  let response = brand_mail::remove_mail_server(&state, &caller, &brand_id).await?;
  Ok(Json(response))
}
