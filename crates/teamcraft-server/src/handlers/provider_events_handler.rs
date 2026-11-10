use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use teamcraft_proto::{
  CreateProviderEventSecretResponse, ErrorBody, GroupMappingsResponse, ProviderEventHookResponse,
  PutGroupMappingsRequest,
};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::{group_mappings, provider_events};
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

/// Identity provider groups mapped to spaces and public channels (docs/provisioning.md §5).
#[utoipa::path(get, path = "/api/v1/admin/identity-provider/group-mappings", tag = "admin", summary = "Identity provider groups mapped to spaces and channels.", responses((status = 200, body = GroupMappingsResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_group_mappings_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<GroupMappingsResponse>, ApiError> {
  Ok(Json(group_mappings::list(&state, &caller).await?))
}

/// Replaces the group mapping; changed rules apply once more to their groups' members.
#[utoipa::path(put, path = "/api/v1/admin/identity-provider/group-mappings", tag = "admin", summary = "Replaces the group mapping.", request_body = PutGroupMappingsRequest, responses((status = 200, body = GroupMappingsResponse), (status = 400, body = ErrorBody, description = "A rule names no space, an unknown space, or a channel that is not public in its spaces"), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_put_group_mappings_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  Json(request): Json<PutGroupMappingsRequest>,
) -> Result<Json<GroupMappingsResponse>, ApiError> {
  Ok(Json(group_mappings::put(&state, &caller, request).await?))
}
