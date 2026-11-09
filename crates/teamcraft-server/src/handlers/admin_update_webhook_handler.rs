use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminWebhook, ErrorBody, UpdateWebhookRequest, WebhookId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::webhooks;
use crate::state::AppState;

/// Changes a webhook's URL, description, events or enabled flag; disabling it empties its queue.
#[utoipa::path(put, path = "/api/v1/admin/webhooks/{webhook_id}", tag = "admin", summary = "Changes an outgoing webhook.", params(("webhook_id" = WebhookId, Path)), request_body = UpdateWebhookRequest, responses((status = 200, body = AdminWebhook), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_update_webhook_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(webhook_id): ApiPath<WebhookId>,
  ApiJson(request): ApiJson<UpdateWebhookRequest>,
) -> Result<Json<AdminWebhook>, ApiError> {
  let response = webhooks::update_webhook(&state, &caller, webhook_id, request).await?;
  Ok(Json(response))
}
