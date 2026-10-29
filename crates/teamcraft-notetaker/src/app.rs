//! Wiring: open (or enroll) the client, the key source, the join tickets, then the worker until a
//! signal.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use secrecy::ExposeSecret;
use teamcraft_core::api::{HttpServerApi, ServerApi, StaticToken};
use teamcraft_core::clock::SystemClock;
use teamcraft_core::records;
use teamcraft_core::store::{LocalStore, SqlCipherStore, StaticKeySource};
use teamcraft_core::{Client, ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_media::call_native::{JoinTicketSource, TicketFuture};
use teamcraft_media::call_resilience::{JoinTicket, JoinTicketError};
use teamcraft_proto::{ConversationId, CreateCallJoinTokenRequest, DeviceId};
use tracing::info;

use crate::config::NotetakerConfig;
use crate::keys::{ClientSettings as OpenRouterSettings, ConfiguredKey, Credentials, KeySource, ServerKey};
use crate::session::SessionLimits;
use crate::summary::SummarySettings;
use crate::worker::{TicketFactory, Worker, WorkerSettings};

pub type NotetakerClient = Client<HttpServerApi<StaticToken>, SqlCipherStore>;

/// Join tickets from the server, over a connection of their own (the worker's client is busy).
struct ServerTickets {
  api: Arc<HttpServerApi<StaticToken>>,
  device_id: DeviceId,
}

/// The tickets of one conversation's call.
struct ConversationTickets {
  api: Arc<HttpServerApi<StaticToken>>,
  conversation: ConversationId,
  device_id: DeviceId,
}

/// Opens the notetaker's client from its local database, enrolling the bot's device on the first
/// run.
pub async fn open_client(config: &NotetakerConfig) -> anyhow::Result<NotetakerClient> {
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
  config: &NotetakerConfig,
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
      .complete(api, store, Box::new(SystemClock), client_settings(), options)
      .await
      .context("enrolling the bot's device")?;
    info!(user_id = %client.user_id(), device_id = %client.device_id(), "enrolled");
    return Ok(client);
  }
  Client::open(api, store, Box::new(SystemClock), client_settings())?
    .context("the local database has an account but it could not be opened")
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
  let tickets = Arc::new(ServerTickets {
    api: Arc::new(HttpServerApi::new(
      &config.server.url,
      StaticToken(token.expose_secret().to_owned()),
    )),
    device_id: client.device_id(),
  });
  let worker = Worker::new(
    client,
    worker_settings(config),
    credentials,
    tickets,
    config.openrouter.max_concurrent_requests,
  );
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

impl TicketFactory for ServerTickets {
  fn for_conversation(&self, conversation: ConversationId) -> Arc<dyn JoinTicketSource> {
    Arc::new(ConversationTickets {
      api: Arc::clone(&self.api),
      conversation,
      device_id: self.device_id,
    })
  }
}

impl JoinTicketSource for ConversationTickets {
  fn fetch(&self) -> TicketFuture {
    let (api, conversation, device_id) = (Arc::clone(&self.api), self.conversation, self.device_id);
    Box::pin(async move {
      let join = api
        .create_call_join_token(conversation, &CreateCallJoinTokenRequest { device_id })
        .await
        .map_err(|error| JoinTicketError {
          reason: error.to_string(),
        })?;
      Ok(JoinTicket::from(join))
    })
  }
}
