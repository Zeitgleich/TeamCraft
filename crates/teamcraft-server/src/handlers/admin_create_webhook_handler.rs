use axum::Json;
use axum::extract::State;
use teamcraft_proto::{CreateWebhookRequest, ErrorBody, WebhookSecretResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::webhooks;
use crate::state::AppState;

/// Creates a webhook; the answer carries its signing secret, the only time it is shown.
#[utoipa::path(post, path = "/api/v1/admin/webhooks", tag = "admin", summary = "Creates an outgoing webhook.", request_body = CreateWebhookRequest, responses((status = 200, body = WebhookSecretResponse), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody), (status = 503, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_create_webhook_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiJson(request): ApiJson<CreateWebhookRequest>,
) -> Result<Json<WebhookSecretResponse>, ApiError> {
  let response = webhooks::create_webhook(&state, &caller, request).await?;
  Ok(Json(response))
}
