//! The sections every bot's configuration file has: how it reaches the server, where its encrypted
//! database is, how its device enrolls, and its liveness file. Each bot adds its own sections and
//! its own environment prefix (`TEAMCRAFT_NOTETAKER_…`, `TEAMCRAFT_RECORDER_…`).

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::SymmetricKey;

/// The TeamCraft server and the bot account the bot acts as.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
  /// Base URL, e.g. `https://chat.example.com`.
  pub url: String,
  /// Bot token from `POST /api/v1/admin/bots`. Prefer the environment or a file.
  #[serde(default)]
  pub bot_token: Option<SecretString>,
  #[serde(default)]
  pub bot_token_file: Option<PathBuf>,
}

/// The encrypted local database (SQLCipher): the bot's keys, its conversations' content keys and
/// what it keeps of the calls it is in.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreSection {
  pub path: PathBuf,
  /// 32 random bytes, base64 (`openssl rand -base64 32`). Prefer the environment or `key_file`.
  #[serde(default)]
  pub key: Option<SecretString>,
  #[serde(default)]
  pub key_file: Option<PathBuf>,
}

/// First-run enrollment of the bot's device.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IdentitySection {
  /// The device's name; empty takes the bot's own default.
  pub device_name: String,
  /// Recovery passphrase that wraps the bot's backup key on the server. Needed on the first run
  /// (and when the local store is lost); keep it somewhere safe.
  pub passphrase: Option<SecretString>,
  pub passphrase_file: Option<PathBuf>,
  /// Argon2id cost of the passphrase; lower it only for tests.
  pub argon2_memory_kib: u32,
  pub argon2_iterations: u32,
}

/// The liveness file the container health check reads: the worker touches it every few seconds
/// while its loop runs.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HealthSection {
  /// Default: `<bot>.alive` next to `store.path`.
  #[serde(rename = "file")]
  pub maybe_file: Option<PathBuf>,
  /// The check fails when the file is older than this.
  pub max_age_secs: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
  #[error("invalid configuration: {0}")]
  Invalid(String),
  #[error("reading {path}: {source}")]
  File { path: String, source: std::io::Error },
  #[error("{0} is not set (directly, by file or in the environment)")]
  MissingSecret(&'static str),
  #[error("store.key must be 32 bytes as base64")]
  BadStoreKey,
}

impl ServerSection {
  pub fn bot_token(&self) -> Result<SecretString, ConfigError> {
    secret("server.bot_token", &self.bot_token, &self.bot_token_file)
  }
}

impl StoreSection {
  /// The database key.
  pub fn key(&self) -> Result<SymmetricKey, ConfigError> {
    let encoded = secret("store.key", &self.key, &self.key_file)?;
    let bytes = [&STANDARD, &URL_SAFE_NO_PAD]
      .iter()
      .find_map(|engine| engine.decode(encoded.expose_secret().trim()).ok())
      .ok_or(ConfigError::BadStoreKey)?;
    SymmetricKey::from_slice(&bytes).ok_or(ConfigError::BadStoreKey)
  }

  /// A file of the bot's next to the database (`file_name` in the same folder).
  pub fn sibling(&self, file_name: &str) -> PathBuf {
    self
      .path
      .parent()
      .unwrap_or_else(|| Path::new(""))
      .join(file_name)
  }
}

impl IdentitySection {
  pub fn passphrase(&self) -> Result<SecretString, ConfigError> {
    secret("identity.passphrase", &self.passphrase, &self.passphrase_file)
  }

  pub fn argon2(&self) -> Argon2Settings {
    Argon2Settings {
      memory_kib: self.argon2_memory_kib,
      iterations: self.argon2_iterations,
      parallelism: 1,
    }
  }

  /// The device's name: the configured one, or `default` when none is set.
  pub fn device_name_or(&self, default: &str) -> String {
    if self.device_name.trim().is_empty() {
      default.to_owned()
    } else {
      self.device_name.clone()
    }
  }
}

impl HealthSection {
  /// Where the liveness file is: `health.file`, or `default_name` next to the database.
  pub fn file(&self, store: &StoreSection, default_name: &str) -> PathBuf {
    self
      .maybe_file
      .clone()
      .unwrap_or_else(|| store.sibling(default_name))
  }

  /// The check reads the file at most this old; the worker rewrites it every 10 s.
  pub fn validate(&self) -> Result<(), ConfigError> {
    if self.max_age_secs < 30 {
      return Err(ConfigError::Invalid(
        "health.max_age_secs must be at least 30 (the file is rewritten every 10 s)".to_owned(),
      ));
    }
    Ok(())
  }
}

impl Default for IdentitySection {
  fn default() -> Self {
    let argon2 = Argon2Settings::default();
    Self {
      device_name: String::new(),
      passphrase: None,
      passphrase_file: None,
      argon2_memory_kib: argon2.memory_kib,
      argon2_iterations: argon2.iterations,
    }
  }
}

impl Default for HealthSection {
  fn default() -> Self {
    Self {
      maybe_file: None,
      max_age_secs: 120,
    }
  }
}

/// A secret given directly or as a file; the file's surrounding whitespace is dropped.
pub fn secret(
  name: &'static str,
  maybe_value: &Option<SecretString>,
  maybe_file: &Option<PathBuf>,
) -> Result<SecretString, ConfigError> {
  if let Some(value) = maybe_value {
    return Ok(value.clone());
  }
  let Some(path) = maybe_file else {
    return Err(ConfigError::MissingSecret(name));
  };
  let text = std::fs::read_to_string(path).map_err(|source| ConfigError::File {
    path: path.display().to_string(),
    source,
  })?;
  Ok(SecretString::from(text.trim().to_owned()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_secret_comes_from_its_value_or_its_file_and_is_missing_otherwise() {
    let direct = secret("x", &Some(SecretString::from("value".to_owned())), &None).unwrap();
    assert_eq!(direct.expose_secret(), "value");
    let path = std::env::temp_dir().join(format!("teamcraft-bot-secret-{}", std::process::id()));
    std::fs::write(&path, "  from-file\n").unwrap();
    let from_file = secret("x", &None, &Some(path.clone())).unwrap();
    assert_eq!(from_file.expose_secret(), "from-file");
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
      secret("x", &None, &None),
      Err(ConfigError::MissingSecret("x"))
    ));
  }

  #[test]
  fn the_store_key_must_be_32_bytes() {
    let store = |key: &str| StoreSection {
      path: PathBuf::from("/var/lib/bot/bot.db"),
      key: Some(SecretString::from(key.to_owned())),
      key_file: None,
    };
    assert!(
      store("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
        .key()
        .is_ok()
    );
    assert!(matches!(store("AAAA").key(), Err(ConfigError::BadStoreKey)));
    assert_eq!(
      store("x").sibling("bot.alive"),
      PathBuf::from("/var/lib/bot/bot.alive")
    );
  }

  #[test]
  fn an_empty_device_name_takes_the_default() {
    let identity = IdentitySection::default();
    assert_eq!(
      identity.device_name_or("teamcraft-recorder"),
      "teamcraft-recorder"
    );
    let named = IdentitySection {
      device_name: "recorder-2".to_owned(),
      ..IdentitySection::default()
    };
    assert_eq!(named.device_name_or("teamcraft-recorder"), "recorder-2");
  }
}
