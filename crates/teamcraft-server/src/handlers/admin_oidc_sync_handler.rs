use axum::Json;
use axum::extract::State;
use teamcraft_proto::{
  AdminOidcSyncPreviewResponse, AdminOidcSyncResponse, ConfirmOidcSyncBrakeRequest, ErrorBody,
};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::oidc_status;
use crate::state::AppState;

/// How the sync with the identity provider stands: last run, errors, what its safety brake holds back.
#[utoipa::path(get, path = "/api/v1/admin/oidc-sync", tag = "admin", summary = "How the sync with the identity provider stands.", responses((status = 200, body = AdminOidcSyncResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_oidc_sync_status_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<AdminOidcSyncResponse>, ApiError> {
  Ok(Json(oidc_status::status(&state, &caller).await?))
}

/// What the sync would change now, without changing anything.
#[utoipa::path(post, path = "/api/v1/admin/oidc-sync/preview", tag = "admin", summary = "What the sync would change now, without changing anything.", responses((status = 200, body = AdminOidcSyncPreviewResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 503, body = ErrorBody, description = "The sync is not configured or the provider cannot be read")), security(("bearer" = [])))]
pub async fn admin_oidc_sync_preview_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<AdminOidcSyncPreviewResponse>, ApiError> {
  Ok(Json(oidc_status::preview(&state, &caller).await?))
}

/// Confirms the deactivations the safety brake holds back; they happen with this run or the next.
#[utoipa::path(post, path = "/api/v1/admin/oidc-sync/brake/confirm", tag = "admin", summary = "Confirms the deactivations the safety brake holds back.", request_body = ConfirmOidcSyncBrakeRequest, responses((status = 200, body = AdminOidcSyncResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody, description = "Owners or admins are among those held back and the caller is no owner"), (status = 409, body = ErrorBody, description = "The list changed since the caller looked at it")), security(("bearer" = [])))]
pub async fn admin_oidc_sync_confirm_brake_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  Json(request): Json<ConfirmOidcSyncBrakeRequest>,
) -> Result<Json<AdminOidcSyncResponse>, ApiError> {
  Ok(Json(oidc_status::confirm_brake(&state, &caller, request).await?))
}
