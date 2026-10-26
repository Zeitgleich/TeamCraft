//! Wiring: open (or enroll) the client, start the endpoints and the worker, stop on a signal.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use secrecy::ExposeSecret;
use teamcraft_core::api::{HttpServerApi, ServerApi, StaticToken};
use teamcraft_core::clock::SystemClock;
use teamcraft_core::records;
use teamcraft_core::store::{LocalStore, SqlCipherStore, StaticKeySource};
use teamcraft_core::{Client, ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use teamcraft_crypto::secret::SecretBytes;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::config::BridgeConfig;
use crate::file_hooks::FileSettings;
use crate::http::{HttpState, Submission, router};
use crate::sources::SourceRegistry;
use crate::worker::{AppSourceSettings, Worker, WorkerSettings};

/// Submissions waiting for the worker; beyond this the endpoints answer 503.
const QUEUE_CAPACITY: usize = 4096;

pub type BridgeClient = Client<HttpServerApi<StaticToken>, SqlCipherStore>;

/// Opens the bridge's client from its local database, enrolling the bot's device on the first run.
pub async fn open_client(config: &BridgeConfig) -> anyhow::Result<BridgeClient> {
  let token = config.bot_token()?;
  let api = HttpServerApi::new(&config.server.url, StaticToken(token.expose_secret().to_owned()));
  let store = SqlCipherStore::open(&config.store.path, &StaticKeySource::new(config.store_key()?))
    .with_context(|| format!("opening the database {}", config.store.path.display()))?;
  enroll_or_open(api, store, config).await
}

/// The part of [`open_client`] that doesn't depend on the concrete API and store.
pub async fn enroll_or_open<A: ServerApi, S: LocalStore>(
  api: A,
  store: S,
  config: &BridgeConfig,
) -> anyhow::Result<Client<A, S>> {
  if records::load_account(&store)?.is_none() {
    info!("no account in the local database: enrolling this device");
    let passphrase = config
      .passphrase()
      .context("the first run needs identity.passphrase to protect the bot's keys")?;
    let options = EnrollmentOptions {
      device_name: config.identity.device_name.clone(),
      secret: EnrollmentSecret::Passphrase(SecretBytes::new(passphrase.expose_secret().as_bytes().to_vec())),
      argon2: config.argon2(),
      maybe_attestation: None,
    };
    let client = PendingEnrollment::new()
      .complete(
        api,
        store,
        Box::new(SystemClock),
        client_settings(config),
        options,
      )
      .await
      .context("enrolling the bot's device")?;
    info!(user_id = %client.user_id(), device_id = %client.device_id(), "enrolled");
    return Ok(client);
  }
  Client::open(api, store, Box::new(SystemClock), client_settings(config))?
    .context("the local database has an account but it could not be opened")
}

/// The core's defaults, with the bridge's file size limit.
fn client_settings(config: &BridgeConfig) -> ClientSettings {
  ClientSettings {
    max_upload_bytes: config.files.max_file_bytes,
    ..ClientSettings::default()
  }
}

/// The running endpoints.
pub struct Endpoints {
  pub addrs: Vec<SocketAddr>,
  stop: watch::Sender<bool>,
  tasks: Vec<JoinHandle<()>>,
}

impl Endpoints {
  /// Stops accepting requests and waits for the ones in flight.
  pub async fn shutdown(self) {
    let _ = self.stop.send(true);
    for task in self.tasks {
      let _ = task.await;
    }
  }
}

/// Binds every configured address and serves the endpoints on them. Failing to bind an IPv6 address
/// is tolerated if another one worked (hosts without IPv6). Files an earlier run left in the spool
/// directory are deleted first.
pub async fn start_endpoints(
  config: &BridgeConfig,
  sources: SourceRegistry,
  sender: mpsc::Sender<Submission>,
) -> anyhow::Result<Endpoints> {
  let files = FileSettings::from_config(config);
  match files.spool.sweep() {
    Ok(0) => {},
    Ok(count) => info!(count, "deleted files an earlier run left in the spool directory"),
    Err(error) => warn!(%error, "the spool directory could not be cleaned up"),
  }
  let state = HttpState::new(sources, sender, files);
  let (stop, stopped) = watch::channel(false);
  let mut listeners: Vec<TcpListener> = Vec::new();
  for addr in &config.listen.addrs {
    match teamcraft_ops::bind_dual_stack(*addr) {
      Ok(listener) => listeners.push(listener),
      Err(error) if addr.is_ipv6() && !listeners.is_empty() => {
        warn!(%addr, %error, "not listening on this address");
      },
      Err(error) => return Err(error).with_context(|| format!("binding {addr}")),
    }
  }
  let mut addrs = Vec::new();
  let mut tasks = Vec::new();
  for listener in listeners {
    addrs.push(listener.local_addr()?);
    let app = router(state.clone(), config.listen.max_body_bytes);
    let mut stopped = stopped.clone();
    tasks.push(tokio::spawn(async move {
      let shutdown = async move {
        let _ = stopped.wait_for(|stop| *stop).await;
      };
      if let Err(error) = axum::serve(listener, app).with_graceful_shutdown(shutdown).await {
        warn!(%error, "an endpoint stopped");
      }
    }));
  }
  Ok(Endpoints { addrs, stop, tasks })
}

/// What the worker takes from the configuration.
pub fn worker_settings(config: &BridgeConfig) -> WorkerSettings {
  WorkerSettings {
    batch: config.batch_limits(),
    index_interval: (config.index.interval_secs > 0).then(|| Duration::from_secs(config.index.interval_secs)),
    maybe_app_sources: config.app_sources.is_enabled.then_some(AppSourceSettings {
      history_pages: config.app_sources.history_pages,
    }),
  }
}

/// Runs the bridge until `shutdown` resolves, then stops the endpoints and sends what is pending.
pub async fn run<A, S>(
  config: &BridgeConfig,
  client: Client<A, S>,
  shutdown: impl std::future::Future<Output = ()>,
) -> anyhow::Result<Client<A, S>>
where
  A: ServerApi,
  S: LocalStore,
{
  let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
  let sources = SourceRegistry::new(config);
  let endpoints = start_endpoints(config, sources.clone(), sender).await?;
  info!(
    addrs = ?endpoints.addrs,
    static_sources = config.sources.len(),
    app_sources = config.app_sources.is_enabled,
    "listening for webhooks and logs"
  );
  let worker = Worker::new(client, sources, receiver, worker_settings(config));
  let work = worker.run();
  tokio::pin!(work);
  tokio::select! {
    client = &mut work => return Ok(client),
    () = shutdown => {},
  }
  info!("shutting down");
  // The worker keeps going while the endpoints drain: a request with files waits for its answer.
  // Once the endpoints are gone, so are all senders, and the worker drains its queue and finishes.
  let ((), client) = tokio::join!(endpoints.shutdown(), work);
  Ok(client)
}
