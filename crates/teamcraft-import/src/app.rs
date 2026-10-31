//! The import device: its encrypted store in the state directory and its enrollment the first time
//! (docs/import-synology.md §7). The identity's passphrase and the store's key are random secrets
//! kept next to the store, readable by this user only; the import account needs no recovery.

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use teamcraft_core::api::HttpServerApi;
use teamcraft_core::clock::SystemClock;
use teamcraft_core::records;
use teamcraft_core::store::{SqlCipherStore, StaticKeySource};
use teamcraft_core::{Client, ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use tracing::info;

use crate::auth::Session;
use crate::files::secret_in;

pub type ImportClient = Client<HttpServerApi<Session>, SqlCipherStore>;

const STORE_FILE: &str = "device.sqlite";
const STORE_KEY_FILE: &str = "store.key";
const PASSPHRASE_FILE: &str = "identity.passphrase";

/// What the import device does differently from an app (e2ee.md §4.10): it waits for people's
/// first devices as long as it runs, makes thumbnails of the export's pictures itself, takes any
/// file size the server takes, and has no notes of its own.
pub fn client_settings() -> ClientSettings {
  ClientSettings {
    pending_addition_expiry: Duration::MAX,
    makes_thumbnails: false,
    creates_notes_to_self: false,
    max_upload_bytes: u64::MAX,
    ..ClientSettings::default()
  }
}

/// Opens the import device, enrolling it on the first run.
pub async fn open_client(state: &Path, session: Session) -> anyhow::Result<ImportClient> {
  let key = secret_in(&state.join(STORE_KEY_FILE)).context("the store key")?;
  let key_bytes = STANDARD.decode(key).context("the store key is not base64")?;
  let key = SymmetricKey::from_slice(&key_bytes).context("the store key has the wrong length")?;
  let store = SqlCipherStore::open(&state.join(STORE_FILE), &StaticKeySource::new(key))
    .context("opening the import device's store")?;
  let api = HttpServerApi::new(session.server_url.clone(), session);
  if records::load_account(&store)?.is_none() {
    info!("enrolling the import device");
    let passphrase = secret_in(&state.join(PASSPHRASE_FILE)).context("the identity passphrase")?;
    let options = EnrollmentOptions {
      device_name: "teamcraft-import".into(),
      secret: EnrollmentSecret::Passphrase(SecretBytes::new(passphrase.into_bytes())),
      argon2: Argon2Settings::default(),
      maybe_attestation: None,
    };
    let client = PendingEnrollment::new()
      .complete(api, store, Box::new(SystemClock), client_settings(), options)
      .await
      .context("enrolling the import device")?;
    info!(user_id = %client.user_id(), device_id = %client.device_id(), "enrolled");
    return Ok(client);
  }
  Client::open(api, store, Box::new(SystemClock), client_settings())?
    .context("the store has an account but it could not be opened")
}
