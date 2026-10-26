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
  ConversationId, DEFAULT_SOURCE_ALERT_LEVELS, DEFAULT_SOURCE_RATE_LIMIT_PER_MINUTE, UserId,
};

use crate::batch::{BatchLimits, Mentions};
use crate::render::LogRules;

const ENV_PREFIX: &str = "TEAMCRAFT_BRIDGE_";
const ENV_NESTING: &str = "__";
/// Environment variable naming the config file; not a config key.
pub const CONFIG_PATH_ENV: &str = "TEAMCRAFT_BRIDGE_CONFIG";
const MIN_SOURCE_TOKEN_CHARS: usize = 16;

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

/// Sources that channel admins add in the app (docs/bridge.md, "Sources").
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
    }
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

  pub fn argon2(&self) -> Argon2Settings {
    Argon2Settings {
      memory_kib: self.identity.argon2_memory_kib,
      iterations: self.identity.argon2_iterations,
      parallelism: 1,
    }
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
      assert!(!config.sources.is_empty());
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
