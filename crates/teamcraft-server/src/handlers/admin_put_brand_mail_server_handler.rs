use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminBrandsResponse, ErrorBody, UpdateBrandMailServerRequest};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::brand_mail;
use crate::state::AppState;

/// Sets a brand's own mail server; an empty host removes it and the password is write-only.
#[utoipa::path(put, path = "/api/v1/admin/brands/{brand_id}/mail-server", tag = "admin", summary = "Sets a brand's own mail server.", params(("brand_id" = String, Path, description = "Brand id")), request_body = UpdateBrandMailServerRequest, responses((status = 200, body = AdminBrandsResponse), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 503, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_put_brand_mail_server_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(brand_id): ApiPath<String>,
  ApiJson(request): ApiJson<UpdateBrandMailServerRequest>,
) -> Result<Json<AdminBrandsResponse>, ApiError> {
  let response = brand_mail::put_mail_server(&state, &caller, &brand_id, request).await?;
  Ok(Json(response))
}
