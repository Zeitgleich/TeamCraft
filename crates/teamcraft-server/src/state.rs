use std::sync::Arc;

use teamcraft_db::{Database, SnowflakeGenerator};
use teamcraft_proto::PermissionScheme;

use crate::auth::{Revocations, TokenValidator};
use secrecy::SecretString;

use crate::config::{
  GatewayConfig, IntegrationConfig, LimitsConfig, NotificationsConfig, PolicyConfig, RendezvousConfig,
  RetentionJobConfig, RoleNames, SecurityConfig, WebConfig,
};
use crate::error::ApiError;
use crate::gateway::Hub;
use crate::meetings::MeetingService;
use crate::push::PushDispatcher;
use crate::rendezvous::RendezvousHub;
use crate::security::RateLimits;
use crate::services::calls::CallService;
use crate::storage::BlobStore;

/// Shared, cheaply cloneable application state of all handlers.
#[derive(Clone)]
pub struct AppState {
  pub database: Database,
  pub validator: Arc<TokenValidator>,
  /// Sessions the identity provider ended (back-channel logout).
  pub revocations: Arc<Revocations>,
  pub ids: Arc<SnowflakeGenerator>,
  pub hub: Arc<Hub>,
  pub rendezvous: Arc<RendezvousHub>,
  pub push: Arc<PushDispatcher>,
  /// Attachment storage, if configured.
  pub maybe_blob_store: Option<Arc<BlobStore>>,
  /// Call join tokens, if calls are configured.
  pub maybe_calls: Option<Arc<CallService>>,
  /// Meetings, if configured.
  pub maybe_meetings: Option<Arc<MeetingService>>,
  pub scheme: Arc<PermissionScheme>,
  pub settings: Arc<Settings>,
  pub rate_limits: Arc<RateLimits>,
}

/// Runtime settings handlers need, taken from the configuration.
#[derive(Debug, Clone, Default)]
pub struct Settings {
  /// The OIDC sync, if enabled.
  pub maybe_oidc_sync: Option<OidcSyncSettings>,
  pub policy: PolicyConfig,
  pub limits: LimitsConfig,
  pub gateway: GatewayConfig,
  pub rendezvous: RendezvousConfig,
  pub retention: RetentionJobConfig,
  pub notifications: NotificationsConfig,
  pub integration: IntegrationConfig,
  pub web: WebConfig,
  pub security: SecurityConfig,
  pub push: PushSettings,
}

impl AppState {
  /// A fresh server-assigned ID.
  pub fn next_id<T: teamcraft_proto::Id>(&self) -> Result<T, ApiError> {
    self.ids.next_id().map_err(|e| ApiError::Internal(e.to_string()))
  }
}

/// Everything the OIDC sync needs to talk to the identity provider.
#[derive(Debug, Clone)]
pub struct OidcSyncSettings {
  pub issuer: String,
  pub token_url: String,
  pub admin_url: String,
  pub client_id: String,
  pub server_client: String,
  pub client_secret: SecretString,
  pub roles: RoleNames,
  pub page_size: u32,
}

/// What the message routing needs from the push configuration.
#[derive(Debug, Clone)]
pub struct PushSettings {
  pub ttl: std::time::Duration,
  pub allow_private_endpoints: bool,
}

impl Default for PushSettings {
  fn default() -> Self {
    Self {
      ttl: std::time::Duration::from_secs(3600),
      allow_private_endpoints: false,
    }
  }
}
