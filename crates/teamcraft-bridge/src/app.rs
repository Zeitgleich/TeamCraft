//! Wiring: open (or enroll) the client, start the endpoints and the worker, stop on a signal.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use secrecy::ExposeSecret;
use teamcraft_core::api::{HttpServerApi, ServerApi, StaticToken};
use teamcraft_core::clock::SystemClock;
use teamcraft_core::records;
use teamcraft_core::store::{LocalStore, SqlCipherStore, StaticKeySource};
use teamcraft_core::{Client, ClientSettings, EnrollmentOptions, PendingEnrollment};
use teamcraft_crypto::secret::SecretBytes;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::config::BridgeConfig;
use crate::http::{HttpState, Submission, router};
use crate::worker::{Worker, WorkerSettings, WorkerSource};

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
      passphrase: SecretBytes::new(passphrase.expose_secret().as_bytes().to_vec()),
      argon2: config.argon2(),
      maybe_attestation: None,
      maybe_org_recovery: None,
      maybe_linked: None,
    };
    let client = PendingEnrollment::new()
      .complete(
        api,
        store,
        Box::new(SystemClock),
        ClientSettings::default(),
        options,
      )
      .await
      .context("enrolling the bot's device")?;
    info!(user_id = %client.user_id(), device_id = %client.device_id(), "enrolled");
    return Ok(client);
  }
  Client::open(api, store, Box::new(SystemClock), ClientSettings::default())?
    .context("the local database has an account but it could not be opened")
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
/// is tolerated if another one worked (hosts without IPv6).
pub async fn start_endpoints(
  config: &BridgeConfig,
  sender: mpsc::Sender<Submission>,
) -> anyhow::Result<Endpoints> {
  let state = HttpState::new(config, sender);
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
pub fn worker_parts(config: &BridgeConfig) -> (Vec<WorkerSource>, WorkerSettings) {
  let sources = config
    .sources
    .iter()
    .map(|source| WorkerSource {
      name: source.name.clone(),
      conversation: source.conversation_id,
      mentions: source.mentions(),
    })
    .collect();
  let settings = WorkerSettings {
    batch: config.batch_limits(),
    index_interval: (config.index.interval_secs > 0).then(|| Duration::from_secs(config.index.interval_secs)),
  };
  (sources, settings)
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
  let endpoints = start_endpoints(config, sender).await?;
  info!(addrs = ?endpoints.addrs, sources = config.sources.len(), "listening for webhooks and logs");
  let (sources, settings) = worker_parts(config);
  let worker = Worker::new(client, sources, receiver, settings);
  let work = worker.run();
  tokio::pin!(work);
  tokio::select! {
    client = &mut work => return Ok(client),
    () = shutdown => {},
  }
  info!("shutting down");
  endpoints.shutdown().await;
  // All senders are gone now, so the worker drains its queue and finishes.
  Ok(work.await)
}
