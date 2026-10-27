//! Bridge configuration: one TOML file plus `TEAMCRAFT_BRIDGE_...` environment overrides
//! (`teamcraft-bridge.example.toml` documents every option).

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use figment::Figment;
use figment::providers::{Env, Format, Toml};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{
  ConversationId, DEFAULT_SOURCE_ALERT_LEVELS, DEFAULT_SOURCE_RATE_LIMIT_PER_MINUTE,
  MAX_SOURCE_FILES_PER_MESSAGE, UserId,
};

use crate::batch::{BatchLimits, Mentions};
use crate::idempotency::{ENTRY_OVERHEAD_BYTES, MAX_IDEMPOTENCY_KEY_BYTES};
use crate::render::LogRules;

const ENV_PREFIX: &str = "TEAMCRAFT_BRIDGE_";
const ENV_NESTING: &str = "__";
/// Environment variable naming the config file; not a config key.
pub const CONFIG_PATH_ENV: &str = "TEAMCRAFT_BRIDGE_CONFIG";
const MIN_SOURCE_TOKEN_CHARS: usize = 16;
/// Smallest `files.max_message_bytes` that leaves room for a text and a few files.
const MIN_MESSAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeConfig {
  pub server: ServerSection,
  pub store: StoreSection,
  #[serde(default)]
  pub identity: IdentitySection,
  #[serde(default)]
  pub listen: ListenSection,
  #[serde(default)]
  pub batching: BatchingSection,
  #[serde(default)]
  pub index: IndexSection,
  #[serde(default)]
  pub app_sources: AppSourcesSection,
  #[serde(default)]
  pub files: FilesSection,
  #[serde(default)]
  pub idempotency: IdempotencySection,
  /// Optional: sources written into this file. Sources added in the app need none.
  #[serde(default)]
  pub sources: Vec<SourceConfig>,
}

/// The TeamCraft server and the bot account the bridge acts as.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
  /// Base URL, e.g. `https://chat.example.com`.
  pub url: String,
  /// Bot token from `POST /api/v1/admin/bots`. Prefer `TEAMCRAFT_BRIDGE_SERVER__BOT_TOKEN` or a file.
  #[serde(default)]
  pub bot_token: Option<SecretString>,
  #[serde(default)]
  pub bot_token_file: Option<PathBuf>,
}

/// The encrypted local database (SQLCipher). It holds the bot's keys and the channels' content keys.
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
  pub device_name: String,
  /// Recovery passphrase that wraps the bot's backup key on the server. Needed on the first run
  /// (and when the local store is lost); keep it somewhere safe.
  pub passphrase: Option<SecretString>,
  pub passphrase_file: Option<PathBuf>,
  /// Argon2id cost of the passphrase; lower it only for tests.
  pub argon2_memory_kib: u32,
  pub argon2_iterations: u32,
}

/// Where the webhook and log endpoints listen.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ListenSection {
  /// Both families on loopback by default. `[::]:8088` is a dual-stack socket for all interfaces.
  pub addrs: Vec<SocketAddr>,
  pub max_body_bytes: usize,
  /// Webhook requests and log lines per minute of all sources together; more are refused or
  /// dropped like a single source's.
  pub total_rate_limit_per_minute: u32,
  /// On shutdown, requests still running after this long are cut off (a producer that stalls
  /// can't hold the bridge up).
  pub drain_timeout_secs: u64,
}

/// How log lines are collected into messages.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BatchingSection {
  /// A batch is sent this long after its first line.
  pub interval_ms: u64,
  /// A batch is sent as soon as its lines add up to this size.
  pub max_bytes: usize,
  /// Longer lines are cut.
  pub max_line_bytes: usize,
}

/// Search index segments the bridge builds for its channels (e2ee.md §10).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IndexSection {
  /// How often closed months are published; `0` turns it off.
  pub interval_secs: u64,
}

/// Sources that workspace admins add in the app (docs/bridge.md, "Sources").
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AppSourcesSection {
  /// Serve them; `false` limits the bridge to the `[[sources]]` of this file.
  #[serde(rename = "enabled")]
  pub is_enabled: bool,
  /// Pages of older history (50 messages each) read once per channel, for sources announced
  /// before the bridge's database existed (a new or lost database).
  pub history_pages: u32,
}

/// Webhooks with files (docs/bridge.md, "Files").
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FilesSection {
  /// Files one webhook message may carry, for sources that don't set their own; `0` takes none.
  pub max_files_per_message: u32,
  /// Largest file, in bytes. The server's attachment limit (`max_attachment_bytes`) applies too.
  pub max_file_bytes: u64,
  /// Bytes of webhooks with files per minute and source; more get `429`. A request can't carry
  /// more than this.
  pub source_bytes_per_minute: u64,
  /// Bytes of webhooks with files per minute, all sources together; more get `429`.
  pub total_bytes_per_minute: u64,
  /// Requests with files at the same time, all sources together and per source; more get `503`
  /// before their body is read.
  pub max_concurrent_requests: u32,
  pub max_concurrent_requests_per_source: u32,
  /// A request whose body sends nothing for this long is cut off (`408`).
  pub idle_timeout_secs: u64,
  /// Bytes in the spool directory at most, all requests together; more get `503`.
  pub max_spool_bytes: u64,
  /// The server's message limit (`[limits] max_ciphertext_bytes`): a message whose text and files
  /// won't fit is refused (`413`) before anything is uploaded.
  pub max_message_bytes: usize,
  /// Pictures larger than this many pixels get no thumbnail (decoding takes about 4 bytes a pixel).
  pub max_thumbnail_pixels: u64,
  /// Where files wait while they go up (plaintext, readable only by the bridge, deleted after the
  /// request). Default: `spool` next to `store.path`.
  #[serde(rename = "spool_dir")]
  pub maybe_spool_dir: Option<PathBuf>,
}

/// Idempotency keys of webhooks (docs/bridge.md, "Retries"): a producer's retry with the key of a
/// request the bridge accepted is answered without posting again. Kept in memory only.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IdempotencySection {
  /// How long the key of an accepted request is remembered, from its answer on.
  pub window_secs: u64,
  /// Keys remembered at most, all sources together; beyond, the oldest are forgotten first (of
  /// the source that holds the most).
  pub max_keys: usize,
  /// Their memory at most: each key counts its length and 128 bytes besides.
  pub max_bytes: usize,
}

/// One producer: a token that selects a conversation, with its limits and alert rules.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
  /// Shown in logs and in the "dropped" notices.
  pub name: String,
  /// Part of the URLs: `/hooks/<token>` and `/ingest/<token>`. At least 16 characters.
  pub token: SecretString,
  pub conversation_id: ConversationId,
  /// Webhook requests and log lines per minute; more are refused or dropped.
  #[serde(default = "default_rate_limit")]
  pub rate_limit_per_minute: u32,
  /// People mentioned by alerts.
  #[serde(default)]
  pub mentions: Vec<MentionConfig>,
  /// Log levels (lower case) that make a line an alert.
  #[serde(default = "default_alert_levels")]
  pub alert_levels: Vec<String>,
  /// Webhook attachments with `color: "danger"` are alerts.
  #[serde(default)]
  pub danger_is_alert: bool,
  /// Files one webhook message may carry; default: `files.max_files_per_message`.
  #[serde(rename = "max_files_per_message", default)]
  pub maybe_max_files_per_message: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MentionConfig {
  pub user_id: UserId,
  /// Written as `@name` in front of an alert.
  pub name: String,
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

impl BridgeConfig {
  /// Reads the file (if any) and the environment, and checks the sources.
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
    let invalid = |message: String| Err(ConfigError::Invalid(message));
    if self.sources.is_empty() && !self.app_sources.is_enabled {
      return invalid("define at least one [[sources]] or turn app_sources on".into());
    }
    if self.listen.addrs.is_empty() {
      return invalid("listen.addrs is empty".into());
    }
    if self.listen.total_rate_limit_per_minute == 0 {
      return invalid("listen.total_rate_limit_per_minute must be above 0".into());
    }
    let mut tokens = HashSet::new();
    let mut names = HashSet::new();
    for source in &self.sources {
      let token = source.token.expose_secret();
      if token.chars().count() < MIN_SOURCE_TOKEN_CHARS
        || !token
          .chars()
          .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
      {
        return invalid(format!(
          "source {}: token needs at least {MIN_SOURCE_TOKEN_CHARS} characters from A-Z a-z 0-9 - _",
          source.name
        ));
      }
      if !tokens.insert(token.to_owned()) {
        return invalid(format!("source {}: the token is used twice", source.name));
      }
      if !names.insert(source.name.clone()) {
        return invalid(format!("source name {} is used twice", source.name));
      }
      if source.rate_limit_per_minute == 0 {
        return invalid(format!(
          "source {}: rate_limit_per_minute must be above 0",
          source.name
        ));
      }
      if source
        .maybe_max_files_per_message
        .is_some_and(|files| files > MAX_SOURCE_FILES_PER_MESSAGE)
      {
        return invalid(format!(
          "source {}: max_files_per_message is at most {MAX_SOURCE_FILES_PER_MESSAGE}",
          source.name
        ));
      }
    }
    self.files.validate()?;
    self.idempotency.validate()?;
    if self.batching.max_bytes < self.batching.max_line_bytes + 16 {
      return invalid("batching.max_bytes must exceed batching.max_line_bytes".into());
    }
    Ok(())
  }

  pub fn batch_limits(&self) -> BatchLimits {
    BatchLimits {
      interval: Duration::from_millis(self.batching.interval_ms),
      max_bytes: self.batching.max_bytes,
      max_line_bytes: self.batching.max_line_bytes,
    }
  }

  pub fn bot_token(&self) -> Result<SecretString, ConfigError> {
    secret(
      "server.bot_token",
      &self.server.bot_token,
      &self.server.bot_token_file,
    )
  }

  pub fn passphrase(&self) -> Result<SecretString, ConfigError> {
    secret(
      "identity.passphrase",
      &self.identity.passphrase,
      &self.identity.passphrase_file,
    )
  }

  /// The database key.
  pub fn store_key(&self) -> Result<SymmetricKey, ConfigError> {
    let encoded = secret("store.key", &self.store.key, &self.store.key_file)?;
    let bytes = [&STANDARD, &URL_SAFE_NO_PAD]
      .iter()
      .find_map(|engine| engine.decode(encoded.expose_secret().trim()).ok())
      .ok_or(ConfigError::BadStoreKey)?;
    SymmetricKey::from_slice(&bytes).ok_or(ConfigError::BadStoreKey)
  }

  /// Where files wait while they go up: `files.spool_dir`, or `spool` next to the database.
  pub fn spool_dir(&self) -> PathBuf {
    self.files.maybe_spool_dir.clone().unwrap_or_else(|| {
      self
        .store
        .path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join("spool")
    })
  }

  pub fn argon2(&self) -> Argon2Settings {
    Argon2Settings {
      memory_kib: self.identity.argon2_memory_kib,
      iterations: self.identity.argon2_iterations,
      parallelism: 1,
    }
  }
}

impl FilesSection {
  fn validate(&self) -> Result<(), ConfigError> {
    let invalid = |message: String| Err(ConfigError::Invalid(message));
    if self.max_files_per_message > MAX_SOURCE_FILES_PER_MESSAGE {
      return invalid(format!(
        "files.max_files_per_message is at most {MAX_SOURCE_FILES_PER_MESSAGE}"
      ));
    }
    if self.max_file_bytes == 0 {
      return invalid("files.max_file_bytes must be above 0".into());
    }
    // Every limit must let at least one file of the largest size through, or a producer gets
    // answers that contradict each other.
    if self.source_bytes_per_minute < self.max_file_bytes
      || self.total_bytes_per_minute < self.source_bytes_per_minute
      || self.max_spool_bytes < self.max_file_bytes
    {
      return invalid(
        "files: max_file_bytes <= source_bytes_per_minute <= total_bytes_per_minute, and \
         max_file_bytes <= max_spool_bytes"
          .into(),
      );
    }
    if self.max_concurrent_requests == 0
      || self.max_concurrent_requests_per_source == 0
      || self.max_concurrent_requests_per_source > self.max_concurrent_requests
    {
      return invalid("files: 0 < max_concurrent_requests_per_source <= max_concurrent_requests".into());
    }
    if self.idle_timeout_secs == 0 {
      return invalid("files.idle_timeout_secs must be above 0".into());
    }
    if self.max_message_bytes < MIN_MESSAGE_BYTES {
      return invalid(format!(
        "files.max_message_bytes must be at least {MIN_MESSAGE_BYTES}"
      ));
    }
    // The core reads the files by path, which it keeps as text.
    if self
      .maybe_spool_dir
      .as_ref()
      .is_some_and(|path| path.to_str().is_none())
    {
      return invalid("files.spool_dir must be valid UTF-8".into());
    }
    Ok(())
  }
}

impl IdempotencySection {
  fn validate(&self) -> Result<(), ConfigError> {
    if self.window_secs == 0 || self.max_keys == 0 {
      return Err(ConfigError::Invalid(
        "idempotency.window_secs and idempotency.max_keys must be above 0".into(),
      ));
    }
    if self.max_bytes < MAX_IDEMPOTENCY_KEY_BYTES + ENTRY_OVERHEAD_BYTES {
      return Err(ConfigError::Invalid(format!(
        "idempotency.max_bytes must hold at least one key ({} bytes)",
        MAX_IDEMPOTENCY_KEY_BYTES + ENTRY_OVERHEAD_BYTES
      )));
    }
    Ok(())
  }

  pub fn window(&self) -> Duration {
    Duration::from_secs(self.window_secs)
  }
}

impl SourceConfig {
  pub fn mentions(&self) -> Mentions {
    Mentions {
      users: self.mentions.iter().map(|mention| mention.user_id).collect(),
      names: self.mentions.iter().map(|mention| mention.name.clone()).collect(),
    }
  }

  pub fn log_rules(&self) -> LogRules {
    LogRules {
      alert_levels: self
        .alert_levels
        .iter()
        .map(|level| level.to_lowercase())
        .collect(),
    }
  }
}

fn secret(
  what: &'static str,
  maybe_direct: &Option<SecretString>,
  maybe_file: &Option<PathBuf>,
) -> Result<SecretString, ConfigError> {
  if let Some(direct) = maybe_direct {
    return Ok(direct.clone());
  }
  let path = maybe_file.as_ref().ok_or(ConfigError::MissingSecret(what))?;
  let text = std::fs::read_to_string(path).map_err(|source| ConfigError::File {
    path: path.display().to_string(),
    source,
  })?;
  Ok(SecretString::from(text.trim().to_owned()))
}

impl Default for IdentitySection {
  fn default() -> Self {
    let argon2 = Argon2Settings::default();
    Self {
      device_name: "teamcraft-bridge".into(),
      passphrase: None,
      passphrase_file: None,
      argon2_memory_kib: argon2.memory_kib,
      argon2_iterations: argon2.iterations,
    }
  }
}

impl Default for ListenSection {
  fn default() -> Self {
    Self {
      addrs: vec![
        SocketAddr::from(([127, 0, 0, 1], 8088)),
        SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 8088)),
      ],
      max_body_bytes: 1024 * 1024,
      total_rate_limit_per_minute: 12_000,
      drain_timeout_secs: 10,
    }
  }
}

impl Default for BatchingSection {
  fn default() -> Self {
    Self {
      interval_ms: 1000,
      max_bytes: 64 * 1024,
      max_line_bytes: 8 * 1024,
    }
  }
}

impl Default for FilesSection {
  fn default() -> Self {
    Self {
      max_files_per_message: 10,
      max_file_bytes: 100 * 1024 * 1024,
      source_bytes_per_minute: 512 * 1024 * 1024,
      total_bytes_per_minute: 1024 * 1024 * 1024,
      max_concurrent_requests: 4,
      max_concurrent_requests_per_source: 2,
      idle_timeout_secs: 30,
      max_spool_bytes: 2 * 1024 * 1024 * 1024,
      max_message_bytes: 1024 * 1024,
      max_thumbnail_pixels: 16_000_000,
      maybe_spool_dir: None,
    }
  }
}

impl Default for IdempotencySection {
  fn default() -> Self {
    Self {
      window_secs: 600,
      max_keys: 10_000,
      max_bytes: 4 * 1024 * 1024,
    }
  }
}

impl Default for AppSourcesSection {
  fn default() -> Self {
    Self {
      is_enabled: true,
      history_pages: 200,
    }
  }
}

impl Default for IndexSection {
  fn default() -> Self {
    Self { interval_secs: 3600 }
  }
}

fn default_rate_limit() -> u32 {
  DEFAULT_SOURCE_RATE_LIMIT_PER_MINUTE
}

fn default_alert_levels() -> Vec<String> {
  DEFAULT_SOURCE_ALERT_LEVELS.map(String::from).into()
}

#[cfg(test)]
mod tests {
  use super::*;

  const MINIMAL: &str = r#"
    [server]
    url = "https://chat.example.com"
    [store]
    path = "bridge.db"
    key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="
    [[sources]]
    name = "nas"
    token = "nas-logs-0123456789"
    conversation_id = "42"
  "#;

  mod loading {
    use super::*;

    #[test]
    fn a_minimal_file_gets_the_defaults() {
      let config = BridgeConfig::from_toml(MINIMAL).unwrap();
      assert_eq!(config.listen.addrs.len(), 2, "loopback on both families");
      assert!(config.listen.addrs.iter().all(|addr| addr.ip().is_loopback()));
      assert_eq!(config.sources[0].rate_limit_per_minute, 600);
      assert!(config.sources[0].alert_levels.contains(&"error".to_owned()));
      assert_eq!(config.batch_limits().interval, Duration::from_secs(1));
      assert_eq!(config.store_key().unwrap().expose_secret()[1], 1);
      assert_eq!(config.files.max_files_per_message, 10);
      assert_eq!(config.sources[0].maybe_max_files_per_message, None);
      assert_eq!(config.spool_dir(), Path::new("spool"), "next to the database");
      assert_eq!(config.idempotency.window(), Duration::from_secs(600));
      assert_eq!(config.idempotency.max_keys, 10_000);
    }

    #[test]
    fn the_spool_directory_can_be_set() {
      let text = format!("{MINIMAL}\n[files]\nspool_dir = \"/tmp/bridge-spool\"\n");
      let config = BridgeConfig::from_toml(&text).unwrap();
      assert_eq!(config.spool_dir(), Path::new("/tmp/bridge-spool"));
      let nested = MINIMAL.replace("path = \"bridge.db\"", "path = \"/var/lib/bridge/bridge.db\"");
      let config = BridgeConfig::from_toml(&nested).unwrap();
      assert_eq!(config.spool_dir(), Path::new("/var/lib/bridge/spool"));
    }

    #[test]
    fn sources_in_the_file_are_optional() {
      let text = MINIMAL.split("[[sources]]").next().unwrap();
      let config = BridgeConfig::from_toml(text).unwrap();
      assert!(config.sources.is_empty());
      assert!(config.app_sources.is_enabled);
      assert_eq!(config.app_sources.history_pages, 200);
    }

    #[test]
    fn unknown_keys_are_rejected() {
      let text = format!("{MINIMAL}\n[extra]\nx = 1\n");
      assert!(matches!(
        BridgeConfig::from_toml(&text),
        Err(ConfigError::Invalid(_))
      ));
    }

    #[test]
    fn the_example_file_loads() {
      let text = include_str!("../teamcraft-bridge.example.toml");
      let config = BridgeConfig::from_toml(text).unwrap();
      assert!(config.sources.is_empty(), "sources come from the app");
      assert!(config.app_sources.is_enabled);
    }
  }

  mod validation {
    use super::*;

    fn with_source(extra: &str) -> String {
      format!("{MINIMAL}\n[[sources]]\n{extra}\n")
    }

    #[test]
    fn weak_duplicate_and_unusable_sources_are_rejected() {
      for (what, text) in [
        (
          "no sources and app sources off",
          format!(
            "{}\n[app_sources]\nenabled = false\n",
            MINIMAL.split("[[sources]]").next().unwrap()
          ),
        ),
        (
          "short token",
          with_source("name = \"b\"\ntoken = \"short\"\nconversation_id = \"1\""),
        ),
        (
          "odd characters",
          with_source("name = \"b\"\ntoken = \"has spaces in the token!\"\nconversation_id = \"1\""),
        ),
        (
          "same token",
          with_source("name = \"b\"\ntoken = \"nas-logs-0123456789\"\nconversation_id = \"1\""),
        ),
        (
          "same name",
          with_source("name = \"nas\"\ntoken = \"another-token-0123456\"\nconversation_id = \"1\""),
        ),
        (
          "no rate",
          with_source(
            "name = \"b\"\ntoken = \"another-token-0123456\"\nconversation_id = \"1\"\nrate_limit_per_minute = 0",
          ),
        ),
        (
          "too many files per message",
          with_source(
            "name = \"b\"\ntoken = \"another-token-0123456\"\nconversation_id = \"1\"\nmax_files_per_message = 17",
          ),
        ),
        (
          "too many files by default",
          format!("{MINIMAL}\n[files]\nmax_files_per_message = 17\n"),
        ),
        (
          "no file bytes",
          format!("{MINIMAL}\n[files]\ntotal_bytes_per_minute = 0\n"),
        ),
        (
          "a source budget below the largest file",
          format!("{MINIMAL}\n[files]\nmax_file_bytes = 2000\nsource_bytes_per_minute = 1000\n"),
        ),
        (
          "a source budget above the total",
          format!("{MINIMAL}\n[files]\nsource_bytes_per_minute = 4294967296\n"),
        ),
        (
          "a spool smaller than a file",
          format!("{MINIMAL}\n[files]\nmax_spool_bytes = 1000\n"),
        ),
        (
          "more per source than in all",
          format!(
            "{MINIMAL}\n[files]\nmax_concurrent_requests = 1\nmax_concurrent_requests_per_source = 2\n"
          ),
        ),
        (
          "no idle timeout",
          format!("{MINIMAL}\n[files]\nidle_timeout_secs = 0\n"),
        ),
        (
          "no idempotency window",
          format!("{MINIMAL}\n[idempotency]\nwindow_secs = 0\n"),
        ),
        (
          "no room for a key",
          format!("{MINIMAL}\n[idempotency]\nmax_bytes = 200\n"),
        ),
      ] {
        assert!(BridgeConfig::from_toml(&text).is_err(), "{what}");
      }
    }
  }

  mod secrets {
    use super::*;

    #[test]
    fn secrets_come_directly_or_from_files_and_are_required() {
      let config = BridgeConfig::from_toml(MINIMAL).unwrap();
      assert!(matches!(config.bot_token(), Err(ConfigError::MissingSecret(_))));
      assert!(matches!(config.passphrase(), Err(ConfigError::MissingSecret(_))));

      let file = std::env::temp_dir().join(format!("bridge-secret-{}", std::process::id()));
      std::fs::write(&file, "tcb_from_file\n").unwrap();
      let with_file = BridgeConfig::from_toml(&MINIMAL.replace(
        "url = \"https://chat.example.com\"",
        &format!(
          "url = \"https://chat.example.com\"\nbot_token_file = {:?}",
          file.display().to_string()
        ),
      ))
      .unwrap();
      assert_eq!(with_file.bot_token().unwrap().expose_secret(), "tcb_from_file");
      std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn a_store_key_must_be_32_bytes() {
      let short = MINIMAL.replace("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=", "AAEC");
      assert!(matches!(
        BridgeConfig::from_toml(&short).unwrap().store_key(),
        Err(ConfigError::BadStoreKey)
      ));
    }
  }
}
