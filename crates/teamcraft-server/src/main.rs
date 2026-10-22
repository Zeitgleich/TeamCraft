//! Entry point: configuration, logging, database, then serve.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use secrecy::ExposeSecret;
use teamcraft_db::{Database, DatabaseOptions, SnowflakeGenerator};
use teamcraft_proto::PermissionScheme;
use teamcraft_server::auth::{
  HttpJwkSource, JwksCache, JwksCacheOptions, RoleMapping, TokenValidator, ValidatorOptions,
};
use teamcraft_server::config::{CONFIG_PATH_ENV, Config};
use teamcraft_server::gateway::{Hub, HubOptions};
use teamcraft_server::listener::bind_listener;
use teamcraft_server::logging;
use teamcraft_server::router::router;
use teamcraft_server::services::presence::refresh_presence;
use teamcraft_server::state::{AppState, Settings};
use tokio::signal;
use tracing::{info, warn};

const SESSION_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

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
    hub: Arc::new(Hub::new(HubOptions {
      resume_window: Duration::from_secs(config.gateway.resume_window_secs),
      buffer_events: config.gateway.resume_buffer_events,
    })),
    scheme: Arc::new(PermissionScheme::default()),
    settings: Arc::new(Settings {
      policy: config.policy.clone(),
      limits: config.limits.clone(),
      gateway: config.gateway.clone(),
    }),
  };
  spawn_session_sweeper(state.clone());

  let listener = bind_listener(config.server.listen_addr)
    .with_context(|| format!("binding {}", config.server.listen_addr))?;
  info!(addr = %config.server.listen_addr, "teamcraft-server listening");
  axum::serve(listener, router(state))
    .with_graceful_shutdown(shutdown_signal())
    .await?;
  info!("shut down");
  Ok(())
}

/// Drops gateway sessions nobody resumed within the window and updates presence for their users.
fn spawn_session_sweeper(state: AppState) {
  tokio::spawn(async move {
    let mut interval = tokio::time::interval(SESSION_SWEEP_INTERVAL);
    loop {
      interval.tick().await;
      for user in state.hub.sweep_expired() {
        if let Err(error) = refresh_presence(&state, user).await {
          warn!(%error, "presence refresh failed");
        }
      }
    }
  });
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
