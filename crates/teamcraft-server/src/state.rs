use std::sync::Arc;

use teamcraft_db::{Database, SnowflakeGenerator};

use crate::auth::TokenValidator;

/// Shared, cheaply cloneable application state of all handlers.
#[derive(Clone)]
pub struct AppState {
  pub database: Database,
  pub validator: Arc<TokenValidator>,
  pub ids: Arc<SnowflakeGenerator>,
}
