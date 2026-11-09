use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SettingsTestResponse, WebhookId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::webhooks;
use crate::state::AppState;

/// Sends a signed `ping` event to the webhook now and reports the answer.
#[utoipa::path(post, path = "/api/v1/admin/webhooks/{webhook_id}/test", tag = "admin", summary = "Sends a test event to an outgoing webhook.", params(("webhook_id" = WebhookId, Path)), responses((status = 200, body = SettingsTestResponse), (status = 404, body = ErrorBody), (status = 503, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_test_webhook_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(webhook_id): ApiPath<WebhookId>,
) -> Result<Json<SettingsTestResponse>, ApiError> {
  let response = webhooks::test_webhook(&state, &caller, webhook_id).await?;
  Ok(Json(response))
}
