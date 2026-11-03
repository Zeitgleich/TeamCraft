use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationResponse, ErrorBody, ImportDmRequest};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::import;
use crate::state::AppState;

/// The DM of two people or one person's notes, with the import account as a member (import account only).
#[utoipa::path(post, path = "/api/v1/admin/import/dms", tag = "admin", summary = "The DM of two people or one person's notes, with the import account as a member (import account only).", request_body = ImportDmRequest, responses((status = 200, body = ConversationResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_import_dm_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiJson(request): ApiJson<ImportDmRequest>,
) -> Result<Json<ConversationResponse>, ApiError> {
  Ok(Json(import::import_dm(&state, &caller, request).await?))
}
