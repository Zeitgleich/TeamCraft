use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use teamcraft_proto::{CreateProviderEventSecretResponse, ErrorBody, ProviderEventHookResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::provider_events;
use crate::state::AppState;

/// `POST /api/v1/identity-provider/events`: the identity provider says something changed
/// (docs/provisioning.md §4). The body is not read for its meaning (at most
/// `MAX_PROVIDER_EVENT_BYTES`, enforced by the route); the secret is the authentication.
pub async fn provider_event_handler(
  State(state): State<AppState>,
  headers: HeaderMap,
  _body: Bytes,
) -> Result<StatusCode, ApiError> {
  let maybe_bearer = headers
    .get(header::AUTHORIZATION)
    .and_then(|value| value.to_str().ok())
    .and_then(|value| value.strip_prefix("Bearer "))
    .map(str::trim);
  provider_events::receive(&state, maybe_bearer).await?;
  Ok(StatusCode::ACCEPTED)
}

/// How the identity provider's event hook stands.
#[utoipa::path(get, path = "/api/v1/admin/identity-provider/event-hook", tag = "admin", summary = "How the identity provider's event hook stands.", responses((status = 200, body = ProviderEventHookResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_provider_event_hook_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<ProviderEventHookResponse>, ApiError> {
  Ok(Json(provider_events::status(&state, &caller).await?))
}

/// A new secret for the identity provider's events, shown this once; the old one stops working.
#[utoipa::path(post, path = "/api/v1/admin/identity-provider/event-hook", tag = "admin", summary = "A new secret for the identity provider's events.", responses((status = 200, body = CreateProviderEventSecretResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_create_provider_event_secret_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<CreateProviderEventSecretResponse>, ApiError> {
  Ok(Json(provider_events::create_secret(&state, &caller).await?))
}

/// Turns the identity provider's event hook off.
#[utoipa::path(delete, path = "/api/v1/admin/identity-provider/event-hook", tag = "admin", summary = "Turns the identity provider's event hook off.", responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_delete_provider_event_hook_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<StatusCode, ApiError> {
  provider_events::delete(&state, &caller).await?;
  Ok(StatusCode::NO_CONTENT)
}
