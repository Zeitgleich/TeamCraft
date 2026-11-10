//! The identity provider's event hook in the admin console (docs/provisioning.md §4): the provider
//! posts "something changed" with a bearer secret, and the server starts a sync run soon.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{UnixMillis, WebhookSecretText};

/// Where the provider posts, under `/api/v1`.
pub const PROVIDER_EVENTS_PATH: &str = "/identity-provider/events";

/// The largest event body the server reads, in bytes; it ignores the content anyway.
pub const MAX_PROVIDER_EVENT_BYTES: usize = 64 * 1024;

/// `GET /api/v1/admin/identity-provider/event-hook`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProviderEventHookResponse {
  /// A secret is set, so the endpoint accepts events.
  pub is_configured: bool,
  #[serde(rename = "created_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_created_at: Option<UnixMillis>,
  /// When an event with the secret last arrived.
  #[serde(rename = "last_event_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_event_at: Option<UnixMillis>,
  /// The sync with the provider runs, which is what an event starts. Without it the endpoint
  /// answers 503.
  pub is_sync_active: bool,
}

/// `POST /api/v1/admin/identity-provider/event-hook`: a new secret, shown this once. Never printed,
/// cleared on drop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateProviderEventSecretResponse {
  pub secret: WebhookSecretText,
  pub hook: ProviderEventHookResponse,
}
