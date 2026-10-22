use std::sync::Arc;

use teamcraft_db::{Database, SnowflakeGenerator};
use teamcraft_proto::PermissionScheme;

use crate::auth::TokenValidator;
use crate::config::{GatewayConfig, LimitsConfig, PolicyConfig};
use crate::error::ApiError;
use crate::gateway::Hub;

/// Shared, cheaply cloneable application state of all handlers.
#[derive(Clone)]
pub struct AppState {
  pub database: Database,
  pub validator: Arc<TokenValidator>,
  pub ids: Arc<SnowflakeGenerator>,
  pub hub: Arc<Hub>,
  pub scheme: Arc<PermissionScheme>,
  pub settings: Arc<Settings>,
}

/// Runtime settings handlers need, taken from the configuration.
#[derive(Debug, Clone, Default)]
pub struct Settings {
  pub policy: PolicyConfig,
  pub limits: LimitsConfig,
  pub gateway: GatewayConfig,
}

impl AppState {
  /// A fresh server-assigned ID.
  pub fn next_id<T: teamcraft_proto::Id>(&self) -> Result<T, ApiError> {
    self.ids.next_id().map_err(|e| ApiError::Internal(e.to_string()))
  }
}
