use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, WebhookId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::webhooks;
use crate::state::AppState;

/// Deletes a webhook with its queued deliveries and its log.
#[utoipa::path(delete, path = "/api/v1/admin/webhooks/{webhook_id}", tag = "admin", summary = "Deletes an outgoing webhook.", params(("webhook_id" = WebhookId, Path)), responses((status = 204), (status = 404, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_delete_webhook_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(webhook_id): ApiPath<WebhookId>,
) -> Result<StatusCode, ApiError> {
  webhooks::delete_webhook(&state, &caller, webhook_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
