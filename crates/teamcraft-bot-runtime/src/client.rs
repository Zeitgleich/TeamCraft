//! Opening a bot's encrypted client from its local database, enrolling the bot's device on the
//! first run (with the recovery passphrase of the configuration).

use secrecy::ExposeSecret;
use teamcraft_core::api::{HttpServerApi, ServerApi, StaticToken};
use teamcraft_core::clock::SystemClock;
use teamcraft_core::records;
use teamcraft_core::store::{LocalStore, SqlCipherStore, StaticKeySource, StoreError};
use teamcraft_core::{
  Client, ClientSettings, CoreError, EnrollmentOptions, EnrollmentSecret, PendingEnrollment,
};
use teamcraft_crypto::secret::SecretBytes;
use thiserror::Error;
use tracing::info;

use crate::config::{ConfigError, IdentitySection, ServerSection, StoreSection};

pub type BotClient = Client<HttpServerApi<StaticToken>, SqlCipherStore>;

/// How a bot opens its client: its configuration and the core's settings it wants.
pub struct OpenParams<'a> {
  pub server: &'a ServerSection,
  pub store: &'a StoreSection,
  pub identity: &'a IdentitySection,
  /// The device's name when the configuration gives none.
  pub default_device_name: &'a str,
  pub settings: ClientSettings,
}

#[derive(Debug, Error)]
pub enum BotClientError {
  #[error(transparent)]
  Config(#[from] ConfigError),
  #[error("opening the database {path}: {source}")]
  Store {
    path: String,
    #[source]
    source: StoreError,
  },
  #[error("the first run needs identity.passphrase to protect the bot's keys: {0}")]
  NoPassphrase(#[source] ConfigError),
  #[error("enrolling the bot's device: {0}")]
  Enrollment(#[source] CoreError),
  #[error("reading the local database: {0}")]
  Core(#[from] CoreError),
  #[error("the local database has an account but it could not be opened")]
  NotOpened,
}

/// Opens the bot's client from its local database, enrolling the bot's device on the first run.
pub async fn open_client(params: OpenParams<'_>) -> Result<BotClient, BotClientError> {
  let token = params.server.bot_token()?;
  let api = HttpServerApi::new(&params.server.url, StaticToken(token.expose_secret().to_owned()));
  let store = SqlCipherStore::open(&params.store.path, &StaticKeySource::new(params.store.key()?)).map_err(
    |source| BotClientError::Store {
      path: params.store.path.display().to_string(),
      source,
    },
  )?;
  enroll_or_open(
    api,
    store,
    params.identity,
    params.default_device_name,
    params.settings,
  )
  .await
}

/// The part of [`open_client`] that doesn't depend on the concrete API and store.
pub async fn enroll_or_open<A: ServerApi, S: LocalStore>(
  api: A,
  store: S,
  identity: &IdentitySection,
  default_device_name: &str,
  settings: ClientSettings,
) -> Result<Client<A, S>, BotClientError> {
  if records::load_account(&store)?.is_none() {
    info!("no account in the local database: enrolling this device");
    let passphrase = identity.passphrase().map_err(BotClientError::NoPassphrase)?;
    let options = EnrollmentOptions {
      device_name: identity.device_name_or(default_device_name),
      secret: EnrollmentSecret::Passphrase(SecretBytes::new(passphrase.expose_secret().as_bytes().to_vec())),
      argon2: identity.argon2(),
      maybe_attestation: None,
    };
    let client = PendingEnrollment::new()
      .complete(api, store, Box::new(SystemClock), settings, options)
      .await
      .map_err(BotClientError::Enrollment)?;
    info!(user_id = %client.user_id(), device_id = %client.device_id(), "enrolled");
    return Ok(client);
  }
  Client::open(api, store, Box::new(SystemClock), settings)?.ok_or(BotClientError::NotOpened)
}
