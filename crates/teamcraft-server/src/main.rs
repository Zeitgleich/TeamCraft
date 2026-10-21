//! Entry point: configuration, logging, database, then serve.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use secrecy::ExposeSecret;
use teamcraft_db::{Database, DatabaseOptions, SnowflakeGenerator};
use teamcraft_server::auth::{
  HttpJwkSource, JwksCache, JwksCacheOptions, RoleMapping, TokenValidator, ValidatorOptions,
};
use teamcraft_server::config::{CONFIG_PATH_ENV, Config};
use teamcraft_server::listener::bind_listener;
use teamcraft_server::logging;
use teamcraft_server::router::router;
use teamcraft_server::state::AppState;
use tokio::signal;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let config = Config::load(config_path().as_deref()).context("loading configuration")?;
  logging::init(&config.logging)?;

  let database = Database::connect(
    config.database.url.expose_secret(),
    &DatabaseOptions {
      max_connections: config.database.max_connections,
      acquire_timeout: config.database.acquire_timeout(),
    },
  )
  .await
  .context("connecting to the database")?;
  database.migrate().await.context("running migrations")?;
  database
    .ensure_partitions(config.server.partitions_ahead_months)
    .await
    .context("creating message partitions")?;

  let state = AppState {
    database,
    validator: Arc::new(build_validator(&config)?),
    ids: Arc::new(SnowflakeGenerator::new(config.server.worker_id).context("invalid server.worker_id")?),
  };

  let listener = bind_listener(config.server.listen_addr)
    .with_context(|| format!("binding {}", config.server.listen_addr))?;
  info!(addr = %config.server.listen_addr, "teamcraft-server listening");
  axum::serve(listener, router(state))
    .with_graceful_shutdown(shutdown_signal())
    .await?;
  info!("shut down");
  Ok(())
}

fn build_validator(config: &Config) -> anyhow::Result<TokenValidator> {
  let oidc = &config.oidc;
  let source = HttpJwkSource::new(
    oidc.issuer.clone(),
    oidc.maybe_jwks_uri.clone(),
    Duration::from_secs(oidc.http_timeout_secs),
  )
  .context("building the HTTP client for the identity provider")?;
  let keys = JwksCache::new(
    Box::new(source),
    JwksCacheOptions {
      ttl: Duration::from_secs(oidc.jwks_cache_ttl_secs),
      min_refresh_interval: Duration::from_secs(oidc.jwks_min_refresh_interval_secs),
    },
  );
  let algorithms = TokenValidator::parse_algorithms(&oidc.algorithms)
    .map_err(|e| anyhow::anyhow!("oidc.algorithms: {e}"))?;
  Ok(TokenValidator::new(
    keys,
    RoleMapping::new(oidc.roles.clone()),
    ValidatorOptions {
      issuer: oidc.issuer.clone(),
      client_id: oidc.client_id.clone(),
      algorithms,
      leeway_secs: oidc.leeway_secs,
    },
  ))
}

/// `--config <path>` or `TEAMCRAFT_CONFIG`; without either only environment variables are used.
fn config_path() -> Option<PathBuf> {
  let mut args = std::env::args().skip(1);
  while let Some(arg) = args.next() {
    if arg == "--config" {
      return args.next().map(PathBuf::from);
    }
    if let Some(path) = arg.strip_prefix("--config=") {
      return Some(PathBuf::from(path));
    }
  }
  std::env::var_os(CONFIG_PATH_ENV).map(PathBuf::from)
}

async fn shutdown_signal() {
  let ctrl_c = async {
    if signal::ctrl_c().await.is_err() {
      std::future::pending::<()>().await;
    }
  };
  #[cfg(unix)]
  let terminate = async {
    match signal::unix::signal(signal::unix::SignalKind::terminate()) {
      Ok(mut stream) => {
        stream.recv().await;
      },
      Err(_) => std::future::pending::<()>().await,
    }
  };
  #[cfg(not(unix))]
  let terminate = std::future::pending::<()>();

  tokio::select! {
    () = ctrl_c => {},
    () = terminate => {},
  }
}
