use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminWebhooksResponse, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::webhooks;
use crate::state::AppState;

/// The outgoing webhooks with their delivery health (docs/webhooks.md).
#[utoipa::path(get, path = "/api/v1/admin/webhooks", tag = "admin", summary = "Lists the outgoing webhooks.", responses((status = 200, body = AdminWebhooksResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_list_webhooks_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<AdminWebhooksResponse>, ApiError> {
  let response = webhooks::list_webhooks(&state, &caller).await?;
  Ok(Json(response))
}
