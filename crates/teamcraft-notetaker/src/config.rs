//! Notetaker configuration: one TOML file plus `TEAMCRAFT_NOTETAKER_...` environment overrides
//! (`teamcraft-notetaker.example.toml` documents every option). What decides whether audio leaves
//! and where to (on/off, models, prompt, language) is not here but in the signed workspace policy
//! (e2ee.md §8.2); this file only says how the notetaker reaches the server and OpenRouter.

use std::path::{Path, PathBuf};
use std::time::Duration;

use figment::Figment;
use figment::providers::{Env, Format, Toml};
use secrecy::SecretString;
use serde::Deserialize;
use teamcraft_bot_runtime::config::secret;
pub use teamcraft_bot_runtime::config::{
  ConfigError, HealthSection, IdentitySection, ServerSection, StoreSection,
};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::SymmetricKey;

use crate::capture::CaptureSettings;
use crate::openrouter::{DEFAULT_BASE_URL, RetrySettings};

const ENV_PREFIX: &str = "TEAMCRAFT_NOTETAKER_";
const ENV_NESTING: &str = "__";
/// Environment variable naming the config file; not a config key.
pub const CONFIG_PATH_ENV: &str = "TEAMCRAFT_NOTETAKER_CONFIG";
/// The device's name when `identity.device_name` is empty.
pub const DEFAULT_DEVICE_NAME: &str = "teamcraft-notetaker";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotetakerConfig {
  pub server: ServerSection,
  pub store: StoreSection,
  #[serde(default)]
  pub identity: IdentitySection,
  #[serde(default)]
  pub openrouter: OpenRouterSection,
  #[serde(default)]
  pub limits: LimitsSection,
  #[serde(default)]
  pub health: HealthSection,
}

/// How OpenRouter is reached. Models, language and prompt are signed policy, not set here.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OpenRouterSection {
  pub base_url: String,
  /// The API key, if it should not be stored on the server. Its SHA-256 must still be the one the
  /// owners signed into the policy. Without it the key comes from the server.
  pub api_key: Option<SecretString>,
  pub api_key_file: Option<PathBuf>,
  /// One request's longest time (upstream providers stop after about 60 seconds).
  pub request_timeout_secs: u64,
  /// Attempts after the first for rate limits (429), timeouts, server errors and lost connections.
  pub max_retries: u32,
  /// First wait before a retry; it doubles with every attempt (or follows `Retry-After`).
  pub retry_base_delay_ms: u64,
  /// Requests at the same time, all calls together.
  pub max_concurrent_requests: usize,
  /// Sent as `X-Title` (OpenRouter's app attribution); empty sends none.
  pub app_title: String,
}

/// Limits of a call and of the pieces of audio sent for transcription.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsSection {
  /// The notetaker leaves a call after this long and posts what it has.
  pub max_call_minutes: u64,
  /// It leaves a call where nobody else has been for this long.
  pub alone_timeout_secs: u64,
  /// It gives up joining a call whose keys it hasn't got after this long.
  pub join_timeout_secs: u64,
  /// Longest piece of one speaker's audio in one request; pieces end earlier at pauses.
  pub max_piece_secs: u64,
  /// A pause this long ends a piece.
  pub pause_ms: u64,
  /// Pieces with less speech than this are not sent (coughs, clicks).
  pub min_speech_ms: u64,
  /// Frames quieter than this count as silence.
  pub voice_threshold_dbfs: f32,
  /// The transcript goes to the summary model in parts of at most this many characters.
  pub summary_chunk_chars: usize,
  /// Most tokens of one summary answer.
  pub max_summary_tokens: u32,
  /// At shutdown, calls in progress get this long to transcribe and post what they have; the rest
  /// is posted at the next start.
  pub shutdown_drain_secs: u64,
}

impl NotetakerConfig {
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
    if !self.openrouter.base_url.starts_with("https://") && !is_loopback_url(&self.openrouter.base_url) {
      return invalid("openrouter.base_url must be https (plain http only to a loopback address)");
    }
    if self.openrouter.max_concurrent_requests == 0 {
      return invalid("openrouter.max_concurrent_requests must be above 0");
    }
    if self.openrouter.request_timeout_secs == 0 {
      return invalid("openrouter.request_timeout_secs must be above 0");
    }
    let limits = &self.limits;
    if limits.max_call_minutes == 0 || limits.alone_timeout_secs == 0 || limits.join_timeout_secs == 0 {
      return invalid("limits: max_call_minutes, alone_timeout_secs and join_timeout_secs must be above 0");
    }
    if !(5..=300).contains(&limits.max_piece_secs) {
      return invalid("limits.max_piece_secs must be 5 to 300");
    }
    if limits.pause_ms < 100 || limits.min_speech_ms > limits.max_piece_secs * 1000 {
      return invalid("limits: pause_ms at least 100, min_speech_ms below max_piece_secs");
    }
    if !(-90.0..=0.0).contains(&limits.voice_threshold_dbfs) {
      return invalid("limits.voice_threshold_dbfs must be -90 to 0");
    }
    if limits.summary_chunk_chars < 4_000 {
      return invalid("limits.summary_chunk_chars must be at least 4000");
    }
    self.health.validate()
  }

  pub fn bot_token(&self) -> Result<SecretString, ConfigError> {
    self.server.bot_token()
  }

  pub fn passphrase(&self) -> Result<SecretString, ConfigError> {
    self.identity.passphrase()
  }

  /// The OpenRouter key of the configuration, if it has one.
  pub fn maybe_api_key(&self) -> Result<Option<SecretString>, ConfigError> {
    match secret(
      "openrouter.api_key",
      &self.openrouter.api_key,
      &self.openrouter.api_key_file,
    ) {
      Ok(key) => Ok(Some(key)),
      Err(ConfigError::MissingSecret(_)) => Ok(None),
      Err(error) => Err(error),
    }
  }

  /// The database key.
  pub fn store_key(&self) -> Result<SymmetricKey, ConfigError> {
    self.store.key()
  }

  pub fn argon2(&self) -> Argon2Settings {
    self.identity.argon2()
  }

  /// Where the liveness file is: `health.file`, or `notetaker.alive` next to the database.
  pub fn health_file(&self) -> PathBuf {
    self.health.file(&self.store, "notetaker.alive")
  }

  pub fn retry_settings(&self) -> RetrySettings {
    RetrySettings {
      max_retries: self.openrouter.max_retries,
      base_delay: Duration::from_millis(self.openrouter.retry_base_delay_ms),
      max_delay: Duration::from_secs(60),
    }
  }

  pub fn capture_settings(&self) -> CaptureSettings {
    CaptureSettings {
      max_piece: Duration::from_secs(self.limits.max_piece_secs),
      pause: Duration::from_millis(self.limits.pause_ms),
      min_speech: Duration::from_millis(self.limits.min_speech_ms),
      voice_threshold_dbfs: self.limits.voice_threshold_dbfs,
    }
  }
}

impl Default for OpenRouterSection {
  fn default() -> Self {
    Self {
      base_url: DEFAULT_BASE_URL.to_owned(),
      api_key: None,
      api_key_file: None,
      request_timeout_secs: 120,
      max_retries: 4,
      retry_base_delay_ms: 1_000,
      max_concurrent_requests: 4,
      app_title: "TeamCraft notetaker".to_owned(),
    }
  }
}

impl Default for LimitsSection {
  fn default() -> Self {
    Self {
      max_call_minutes: 240,
      alone_timeout_secs: 60,
      join_timeout_secs: 60,
      max_piece_secs: 30,
      pause_ms: 700,
      min_speech_ms: 500,
      voice_threshold_dbfs: -50.0,
      summary_chunk_chars: 48_000,
      max_summary_tokens: 2_000,
      shutdown_drain_secs: 60,
    }
  }
}

fn is_loopback_url(url: &str) -> bool {
  ["http://127.0.0.1", "http://[::1]", "http://localhost"]
    .iter()
    .any(|prefix| url.starts_with(prefix))
}

#[cfg(test)]
mod tests {
  use secrecy::ExposeSecret;

  use super::*;

  const MINIMAL: &str = r#"
    [server]
    url = "https://chat.example.com"
    bot_token = "tcb_x"
    [store]
    path = "notetaker.db"
    key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
  "#;

  #[test]
  fn a_minimal_file_gets_the_documented_defaults() {
    let config = NotetakerConfig::from_toml(MINIMAL).unwrap();
    assert_eq!(config.openrouter.base_url, "https://openrouter.ai/api/v1");
    assert_eq!(config.limits.max_piece_secs, 30);
    assert_eq!(config.limits.max_call_minutes, 240);
    assert!(config.maybe_api_key().unwrap().is_none());
    assert!(config.store_key().is_ok());
    assert_eq!(config.bot_token().unwrap().expose_secret(), "tcb_x");
  }

  #[test]
  fn plain_http_is_refused_except_to_loopback() {
    let remote = format!("{MINIMAL}\n[openrouter]\nbase_url = \"http://openrouter.example.com/api/v1\"\n");
    assert!(matches!(
      NotetakerConfig::from_toml(&remote),
      Err(ConfigError::Invalid(_))
    ));
    let local = format!("{MINIMAL}\n[openrouter]\nbase_url = \"http://127.0.0.1:9000/api/v1\"\n");
    assert!(NotetakerConfig::from_toml(&local).is_ok());
  }

  #[test]
  fn out_of_range_limits_are_refused() {
    for limits in [
      "max_piece_secs = 2",
      "max_call_minutes = 0",
      "voice_threshold_dbfs = 3.0",
      "summary_chunk_chars = 100",
      "pause_ms = 10",
    ] {
      let text = format!("{MINIMAL}\n[limits]\n{limits}\n");
      assert!(NotetakerConfig::from_toml(&text).is_err(), "{limits}");
    }
  }

  #[test]
  fn the_example_file_parses_with_the_defaults() {
    let example = NotetakerConfig::from_toml(include_str!("../teamcraft-notetaker.example.toml")).unwrap();
    let defaults = NotetakerConfig::from_toml(MINIMAL).unwrap();
    assert_eq!(format!("{:?}", example.limits), format!("{:?}", defaults.limits));
    assert_eq!(example.openrouter.max_retries, defaults.openrouter.max_retries);
    assert_eq!(example.openrouter.app_title, defaults.openrouter.app_title);
  }

  #[test]
  fn unknown_keys_are_refused() {
    let text = format!("{MINIMAL}\n[openrouter]\nmodel = \"x/y\"\n");
    assert!(NotetakerConfig::from_toml(&text).is_err());
  }
}
