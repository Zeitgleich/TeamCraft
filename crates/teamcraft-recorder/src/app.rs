//! Wiring: open (or enroll) the client, the join tickets, then the worker until a signal.

use std::sync::Arc;
use std::time::Duration;

use teamcraft_bot_runtime::client::{BotClient, OpenParams};
use teamcraft_bot_runtime::health::Liveness;
use teamcraft_bot_runtime::tickets::ServerTickets;
use teamcraft_core::ClientSettings;
use tracing::info;

use crate::config::{DEFAULT_DEVICE_NAME, RecorderConfig};
use crate::worker::{Worker, WorkerSettings};

/// Opens the recorder's client from its local database, enrolling the bot's device on the first
/// run.
pub async fn open_client(config: &RecorderConfig) -> anyhow::Result<BotClient> {
  let client = teamcraft_bot_runtime::client::open_client(OpenParams {
    server: &config.server,
    store: &config.store,
    identity: &config.identity,
    default_device_name: DEFAULT_DEVICE_NAME,
    settings: client_settings(config),
  })
  .await?;
  Ok(client)
}

/// Runs the recorder until `stop` resolves.
pub async fn run(
  config: &RecorderConfig,
  client: BotClient,
  stop: impl Future<Output = ()>,
) -> anyhow::Result<()> {
  let token = config.server.bot_token()?;
  let tickets = Arc::new(ServerTickets::new(&config.server.url, &token, client.device_id()));
  let worker =
    Worker::new(client, worker_settings(config), tickets).with_liveness(Liveness::new(config.health_file()));
  info!("recorder running");
  worker.run_until(stop).await;
  Ok(())
}

/// What the worker takes from the configuration.
pub fn worker_settings(config: &RecorderConfig) -> WorkerSettings {
  WorkerSettings {
    limits: config.session_limits(),
    folder: config.recordings_folder(),
    staging: config.staging_folder(),
    min_free_bytes: config.min_free_bytes(),
    language: config.recordings.language.clone(),
    shutdown_drain: Duration::from_secs(config.limits.shutdown_drain_secs),
  }
}

/// The core's defaults; the recorder posts audio only and makes no thumbnails. Its decrypted copies
/// wait in the staging folder, which the core empties as the uploads finish.
fn client_settings(config: &RecorderConfig) -> ClientSettings {
  ClientSettings {
    makes_thumbnails: false,
    maybe_upload_staging_dir: Some(config.staging_folder()),
    ..ClientSettings::default()
  }
}
