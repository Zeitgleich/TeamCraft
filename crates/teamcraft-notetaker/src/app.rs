//! Wiring: open (or enroll) the client (`teamcraft-bot-runtime`), the key source, the join
//! tickets, then the worker until a signal.

use std::sync::Arc;
use std::time::Duration;

use teamcraft_bot_runtime::client::{BotClient, OpenParams};
use teamcraft_bot_runtime::tickets::ServerTickets;
use teamcraft_core::ClientSettings;
use tracing::info;

use crate::config::{DEFAULT_DEVICE_NAME, NotetakerConfig};
use crate::health::Liveness;
use crate::keys::{ClientSettings as OpenRouterSettings, ConfiguredKey, Credentials, KeySource, ServerKey};
use crate::session::SessionLimits;
use crate::summary::SummarySettings;
use crate::worker::{Worker, WorkerSettings};

pub type NotetakerClient = BotClient;

/// Opens the notetaker's client from its local database, enrolling the bot's device on the first
/// run.
pub async fn open_client(config: &NotetakerConfig) -> anyhow::Result<NotetakerClient> {
  let client = teamcraft_bot_runtime::client::open_client(OpenParams {
    server: &config.server,
    store: &config.store,
    identity: &config.identity,
    default_device_name: DEFAULT_DEVICE_NAME,
    settings: client_settings(),
  })
  .await?;
  Ok(client)
}

/// Runs the notetaker until `stop` resolves.
pub async fn run(
  config: &NotetakerConfig,
  client: NotetakerClient,
  stop: impl Future<Output = ()>,
) -> anyhow::Result<()> {
  let token = config.bot_token()?;
  let key_source: Arc<dyn KeySource> = match config.maybe_api_key()? {
    Some(key) => {
      info!("using the OpenRouter key of the configuration");
      Arc::new(ConfiguredKey(key))
    },
    None => Arc::new(ServerKey::new(&config.server.url, token.clone())?),
  };
  let credentials = Credentials::new(
    key_source,
    OpenRouterSettings {
      base_url: config.openrouter.base_url.clone(),
      request_timeout: Duration::from_secs(config.openrouter.request_timeout_secs),
      retry: config.retry_settings(),
      maybe_app_title: Some(config.openrouter.app_title.clone()),
    },
  );
  let tickets = Arc::new(ServerTickets::new(&config.server.url, &token, client.device_id()));
  let worker = Worker::new(
    client,
    worker_settings(config),
    credentials,
    tickets,
    config.openrouter.max_concurrent_requests,
  )
  .with_liveness(Liveness::new(config.health_file()));
  info!("notetaker running");
  worker.run_until(stop).await;
  Ok(())
}

/// What the worker takes from the configuration.
pub fn worker_settings(config: &NotetakerConfig) -> WorkerSettings {
  WorkerSettings {
    capture: config.capture_settings(),
    limits: SessionLimits {
      max_call: Duration::from_secs(config.limits.max_call_minutes * 60),
      alone: Duration::from_secs(config.limits.alone_timeout_secs),
      join: Duration::from_secs(config.limits.join_timeout_secs),
    },
    summary: SummarySettings {
      chunk_chars: config.limits.summary_chunk_chars,
      max_tokens: config.limits.max_summary_tokens,
    },
    shutdown_drain: Duration::from_secs(config.limits.shutdown_drain_secs),
  }
}

/// The core's defaults; the notetaker posts small files only and makes no thumbnails.
fn client_settings() -> ClientSettings {
  ClientSettings {
    makes_thumbnails: false,
    ..ClientSettings::default()
  }
}
