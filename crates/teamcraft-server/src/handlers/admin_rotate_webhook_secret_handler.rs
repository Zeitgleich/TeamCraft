use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, WebhookId, WebhookSecretResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::webhooks;
use crate::state::AppState;

/// Replaces a webhook's signing secret; the old one stops working at once.
#[utoipa::path(post, path = "/api/v1/admin/webhooks/{webhook_id}/secret", tag = "admin", summary = "Rotates the signing secret of an outgoing webhook.", params(("webhook_id" = WebhookId, Path)), responses((status = 200, body = WebhookSecretResponse), (status = 404, body = ErrorBody), (status = 503, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_rotate_webhook_secret_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(webhook_id): ApiPath<WebhookId>,
) -> Result<Json<WebhookSecretResponse>, ApiError> {
  let response = webhooks::rotate_secret(&state, &caller, webhook_id).await?;
  Ok(Json(response))
}
