use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, WebhookDeliveriesResponse, WebhookId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::webhooks;
use crate::state::AppState;

/// The latest delivery attempts of a webhook, newest first.
#[utoipa::path(get, path = "/api/v1/admin/webhooks/{webhook_id}/deliveries", tag = "admin", summary = "Lists the latest deliveries of an outgoing webhook.", params(("webhook_id" = WebhookId, Path)), responses((status = 200, body = WebhookDeliveriesResponse), (status = 404, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_list_webhook_deliveries_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(webhook_id): ApiPath<WebhookId>,
) -> Result<Json<WebhookDeliveriesResponse>, ApiError> {
  let response = webhooks::list_deliveries(&state, &caller, webhook_id).await?;
  Ok(Json(response))
}
