//! Recorder configuration: one TOML file plus `TEAMCRAFT_RECORDER_...` environment overrides
//! (`teamcraft-recorder.example.toml` documents every option). Whether the bot may record at all,
//! and for how long, is not here but in the signed workspace policy (e2ee.md §8.3, admin.md §3.13);
//! this file only says how the bot reaches the server and where it writes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use figment::Figment;
use figment::providers::{Env, Format, Toml};
use serde::Deserialize;
pub use teamcraft_bot_runtime::config::{
  ConfigError, HealthSection, IdentitySection, ServerSection, StoreSection,
};

use crate::session::SessionLimits;

const ENV_PREFIX: &str = "TEAMCRAFT_RECORDER_";
const ENV_NESTING: &str = "__";
/// Environment variable naming the config file; not a config key.
pub const CONFIG_PATH_ENV: &str = "TEAMCRAFT_RECORDER_CONFIG";
/// The device's name when `identity.device_name` is empty.
pub const DEFAULT_DEVICE_NAME: &str = "teamcraft-recorder";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecorderConfig {
  pub server: ServerSection,
  pub store: StoreSection,
  #[serde(default)]
  pub identity: IdentitySection,
  #[serde(default)]
  pub recordings: RecordingsSection,
  #[serde(default)]
  pub limits: LimitsSection,
  #[serde(default)]
  pub health: HealthSection,
}

/// Where the takes are written and what is posted with them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RecordingsSection {
  /// The sealed takes while a call is recorded. Default: `recordings` next to `store.path`.
  #[serde(rename = "folder")]
  pub maybe_folder: Option<PathBuf>,
  /// The decrypted copies waiting for their upload (deleted once uploaded). Default: `uploads`
  /// next to `store.path`. Keep it on the same protected volume as the database.
  #[serde(rename = "staging")]
  pub maybe_staging: Option<PathBuf>,
  /// A take doesn't start with less free space than this on the recordings' disk, in MiB (an hour
  /// is about 14 MB, and posting needs as much again for the decrypted copy).
  pub min_free_mib: u64,
  /// The language of the posted texts: `de` or `en` (English for anything else).
  pub language: String,
}

/// Limits of a call.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsSection {
  /// The bot leaves a call where nobody else has been for this long, and posts what it has.
  pub alone_timeout_secs: u64,
  /// It gives up joining a call whose keys it hasn't got after this long.
  pub join_timeout_secs: u64,
  /// It stops recording when the SFU hasn't confirmed its recording notice for this long: the
  /// others may not see the recording, and the bot never records unannounced.
  pub unconfirmed_notice_secs: u64,
  /// At shutdown, calls in progress get this long to finish their takes and queue them for
  /// posting; a take not finished by then is lost.
  pub shutdown_drain_secs: u64,
}

impl RecorderConfig {
  /// Reads the file (if any) and the environment, and checks the values.
  pub fn load(maybe_path: Option<&Path>) -> Result<Self, ConfigError> {
    let mut figment = Figment::new();
    if let Some(path) = maybe_path {
      figment = figment.merge(Toml::file(path));
    }
    figment = figment.merge(Env::prefixed(ENV_PREFIX).split(ENV_NESTING).ignore(&["config"]));
    Self::from_figment(&figment)
  }

  /// Parses TOML text (tests and tools).
  pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
    Self::from_figment(&Figment::new().merge(Toml::string(text)))
  }

  fn from_figment(figment: &Figment) -> Result<Self, ConfigError> {
    let config: Self = figment
      .extract()
      .map_err(|error| ConfigError::Invalid(error.to_string()))?;
    config.validate()?;
    Ok(config)
  }

  fn validate(&self) -> Result<(), ConfigError> {
    let invalid = |message: &str| Err(ConfigError::Invalid(message.to_owned()));
    let limits = &self.limits;
    if limits.alone_timeout_secs == 0 || limits.join_timeout_secs == 0 {
      return invalid("limits: alone_timeout_secs and join_timeout_secs must be above 0");
    }
    // The call engine warns after 6 s without a confirmation; give it a few resends more.
    if limits.unconfirmed_notice_secs < 10 {
      return invalid("limits.unconfirmed_notice_secs must be at least 10");
    }
    self.health.validate()
  }

  /// Where sealed takes are written.
  pub fn recordings_folder(&self) -> PathBuf {
    self
      .recordings
      .maybe_folder
      .clone()
      .unwrap_or_else(|| self.store.sibling("recordings"))
  }

  /// Where decrypted copies wait for their upload.
  pub fn staging_folder(&self) -> PathBuf {
    self
      .recordings
      .maybe_staging
      .clone()
      .unwrap_or_else(|| self.store.sibling("uploads"))
  }

  /// The free-space floor, in bytes.
  pub fn min_free_bytes(&self) -> u64 {
    self.recordings.min_free_mib.saturating_mul(1024 * 1024)
  }

  /// Where the liveness file is: `health.file`, or `recorder.alive` next to the database.
  pub fn health_file(&self) -> PathBuf {
    self.health.file(&self.store, "recorder.alive")
  }

  pub fn session_limits(&self) -> SessionLimits {
    SessionLimits {
      alone: Duration::from_secs(self.limits.alone_timeout_secs),
      join: Duration::from_secs(self.limits.join_timeout_secs),
      unconfirmed: Duration::from_secs(self.limits.unconfirmed_notice_secs),
    }
  }
}

impl Default for RecordingsSection {
  fn default() -> Self {
    Self {
      maybe_folder: None,
      maybe_staging: None,
      min_free_mib: 1024,
      language: "en".to_owned(),
    }
  }
}

impl Default for LimitsSection {
  fn default() -> Self {
    Self {
      alone_timeout_secs: 60,
      join_timeout_secs: 60,
      unconfirmed_notice_secs: 30,
      shutdown_drain_secs: 30,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const MINIMAL: &str = r#"
    [server]
    url = "https://chat.example.com"
    bot_token = "tcb_x"
    [store]
    path = "/var/lib/teamcraft-recorder/recorder.db"
    key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
  "#;

  #[test]
  fn a_minimal_file_gets_the_documented_defaults() {
    let config = RecorderConfig::from_toml(MINIMAL).unwrap();
    assert_eq!(
      config.recordings_folder(),
      PathBuf::from("/var/lib/teamcraft-recorder/recordings")
    );
    assert_eq!(
      config.staging_folder(),
      PathBuf::from("/var/lib/teamcraft-recorder/uploads")
    );
    assert_eq!(config.min_free_bytes(), 1024 * 1024 * 1024);
    assert_eq!(
      config.health_file(),
      PathBuf::from("/var/lib/teamcraft-recorder/recorder.alive")
    );
    assert_eq!(config.limits.alone_timeout_secs, 60);
    assert_eq!(
      config.identity.device_name_or(DEFAULT_DEVICE_NAME),
      DEFAULT_DEVICE_NAME
    );
    assert!(config.store.key().is_ok());
  }

  #[test]
  fn out_of_range_limits_and_unknown_keys_are_refused() {
    for extra in [
      "[limits]\nalone_timeout_secs = 0",
      "[limits]\nunconfirmed_notice_secs = 2",
      "[health]\nmax_age_secs = 5",
      "[recordings]\nformat = \"wav\"",
    ] {
      let text = format!("{MINIMAL}\n{extra}\n");
      assert!(RecorderConfig::from_toml(&text).is_err(), "{extra}");
    }
  }

  #[test]
  fn the_example_file_parses_with_the_defaults() {
    let example = RecorderConfig::from_toml(include_str!("../teamcraft-recorder.example.toml")).unwrap();
    let defaults = RecorderConfig::from_toml(MINIMAL).unwrap();
    assert_eq!(format!("{:?}", example.limits), format!("{:?}", defaults.limits));
    assert_eq!(example.recordings.min_free_mib, defaults.recordings.min_free_mib);
    assert_eq!(example.recordings.language, defaults.recordings.language);
  }
}
