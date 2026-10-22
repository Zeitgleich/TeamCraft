use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use figment::Figment;
use figment::providers::{Env, Format, Toml};
use secrecy::SecretString;
use serde::Deserialize;

/// Prefix of environment overrides. Nested keys are separated by a double underscore:
/// `TEAMCRAFT_OIDC__ISSUER`, `TEAMCRAFT_OIDC__ROLES__GUEST`, `TEAMCRAFT_DATABASE__URL`.
const ENV_PREFIX: &str = "TEAMCRAFT_";
const ENV_NESTING: &str = "__";
/// Environment variable naming the config file itself; not a config key.
pub const CONFIG_PATH_ENV: &str = "TEAMCRAFT_CONFIG";

/// Complete server configuration. `teamcraft.example.toml` documents every option.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
  #[serde(default)]
  pub server: ServerConfig,
  pub database: DatabaseConfig,
  pub oidc: OidcConfig,
  #[serde(default)]
  pub logging: LoggingConfig,
  #[serde(default)]
  pub policy: PolicyConfig,
  #[serde(default)]
  pub limits: LimitsConfig,
  #[serde(default)]
  pub gateway: GatewayConfig,
  #[serde(default)]
  pub rendezvous: RendezvousConfig,
  /// Blob storage for attachments; without it the attachment endpoints answer 503.
  #[serde(default)]
  pub storage: Option<StorageConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
  /// Address of the HTTP listener. TLS is terminated by the reverse proxy. The default `[::]` is a
  /// dual-stack socket (IPv4 and IPv6); set e.g. `127.0.0.1:8080` to bind a single address.
  pub listen_addr: SocketAddr,
  /// Unique number (0-1023) of this server instance in generated Snowflake IDs.
  pub worker_id: u16,
  /// How many months of message partitions are created ahead of time.
  pub partitions_ahead_months: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
  /// `postgres://user:password@host:5432/database`
  pub url: SecretString,
  #[serde(default = "default_max_connections")]
  pub max_connections: u32,
  #[serde(default = "default_acquire_timeout_secs")]
  pub acquire_timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcConfig {
  /// Issuer URL; must equal the `iss` claim of the tokens and the issuer in the discovery document.
  pub issuer: String,
  /// Public client the apps log in with. Tokens must be issued to it (`azp` or `aud`), and its
  /// client roles are read from `resource_access.<client_id>.roles`.
  #[serde(default = "default_client_id")]
  pub client_id: String,
  /// Confidential client with a service account, reserved for the admin API and back-channel logout.
  #[serde(default = "default_server_client")]
  pub server_client: String,
  /// Overrides the `jwks_uri` of the discovery document (e.g. an internal URL, or no discovery).
  #[serde(rename = "jwks_uri", default)]
  pub maybe_jwks_uri: Option<String>,
  /// Accepted JWS algorithms, e.g. `RS256`, `ES256`, `EdDSA`.
  #[serde(default = "default_algorithms")]
  pub algorithms: Vec<String>,
  /// Tolerated clock difference when checking `exp` and `nbf`.
  #[serde(default = "default_leeway_secs")]
  pub leeway_secs: u64,
  /// How long fetched signing keys are used before they are refreshed.
  #[serde(default = "default_jwks_cache_ttl_secs")]
  pub jwks_cache_ttl_secs: u64,
  /// Minimum time between two key fetches, also when a token names an unknown key ID.
  #[serde(default = "default_jwks_min_refresh_interval_secs")]
  pub jwks_min_refresh_interval_secs: u64,
  /// Timeout of requests to the identity provider.
  #[serde(default = "default_http_timeout_secs")]
  pub http_timeout_secs: u64,
  #[serde(default)]
  pub roles: RoleNames,
}

/// Names of the identity provider's client roles per TeamCraft role (ARCHITECTURE.md §5.1.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RoleNames {
  pub owner: String,
  pub admin: String,
  pub member: String,
  pub guest: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LoggingConfig {
  /// `tracing` filter directive, e.g. `info` or `teamcraft_server=debug,info`. `RUST_LOG` wins.
  pub filter: String,
  pub format: LogFormat,
}

/// Instance policies with defaults (PLAN.md §12, §13). Later they move into signed, runtime-editable policy.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PolicyConfig {
  /// Guests can only open DMs with people they share a conversation with.
  pub guests_dm_only_with_shared_conversations: bool,
  /// Pending invites only apply to accounts whose identity provider verified the email address.
  pub invites_require_verified_email: bool,
}

/// Size limits of requests and results.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsConfig {
  pub max_ciphertext_bytes: usize,
  pub max_mentions_per_message: usize,
  pub max_group_dm_participants: usize,
  pub max_name_length: usize,
  pub max_text_length: usize,
  pub default_page_size: u32,
  pub max_page_size: u32,
  pub max_directory_results: u32,
  /// Unused key packages a device may have on the server.
  pub max_key_packages_per_device: usize,
  pub max_key_package_bytes: usize,
  /// Key packages in one upload.
  pub max_key_packages_per_upload: usize,
  /// Size of one MLS commit, welcome, proposal or application message.
  pub max_mls_payload_bytes: usize,
  pub max_welcomes_per_commit: usize,
  /// Encrypted key bag plus wrapped backup keys of one user.
  pub max_key_bag_bytes: usize,
}

/// WebSocket gateway behaviour.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GatewayConfig {
  /// Interval the client must heartbeat at.
  pub heartbeat_interval_secs: u64,
  /// A connection without any frame for this long is closed.
  pub heartbeat_timeout_secs: u64,
  /// How long a disconnected session can be resumed.
  pub resume_window_secs: u64,
  /// Events kept per session for resuming.
  pub resume_buffer_events: usize,
  /// Time a new connection has to identify.
  pub identify_timeout_secs: u64,
  /// Connections are closed after this long so that a fresh token is checked; clients resume.
  pub connection_max_age_secs: u64,
  /// A device that disconnected only counts as offline after this grace period (reconnects are common).
  pub presence_offline_grace_secs: u64,
  /// Largest accepted client frame.
  pub max_frame_bytes: usize,
}

/// S3-compatible blob storage (Garage in development). Everything stored there is encrypted by the
/// clients; the provider is untrusted.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
  /// Endpoint URL clients can reach (they upload and download through presigned URLs).
  pub endpoint: String,
  #[serde(default = "default_region")]
  pub region: String,
  pub bucket: String,
  pub access_key: String,
  pub secret_key: SecretString,
  /// Path-style addressing (`endpoint/bucket/key`), needed by Garage and MinIO.
  #[serde(default = "default_true")]
  pub path_style: bool,
  /// Validity of presigned URLs.
  #[serde(default = "default_presign_expiry_secs")]
  pub presign_expiry_secs: u64,
  /// Largest attachment (ciphertext) a client may announce.
  #[serde(default = "default_max_attachment_bytes")]
  pub max_attachment_bytes: u64,
}

/// Device-linking relay at /api/v1/rendezvous.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RendezvousConfig {
  /// Lifetime of a relay from the first join.
  pub ttl_secs: u64,
  /// Largest relayed binary frame.
  pub max_frame_bytes: usize,
  /// Total bytes relayed per rendezvous, both directions.
  pub max_total_bytes: usize,
  /// Frames held for a device whose peer has not joined yet.
  pub max_pending_frames: usize,
  /// Open relays per user.
  pub max_per_user: usize,
  /// Time a new connection has to join.
  pub join_timeout_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
  Pretty,
  Json,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
  #[error("invalid configuration: {0}")]
  Invalid(#[from] Box<figment::Error>),
  #[error("config file {0} does not exist")]
  MissingFile(String),
}

impl Config {
  /// Loads the optional TOML file and applies `TEAMCRAFT_` environment overrides on top.
  pub fn load(maybe_path: Option<&Path>) -> Result<Self, ConfigError> {
    if let Some(path) = maybe_path
      && !path.is_file()
    {
      return Err(ConfigError::MissingFile(path.display().to_string()));
    }
    let mut figment = Figment::new();
    if let Some(path) = maybe_path {
      figment = figment.merge(Toml::file(path));
    }
    Self::from_figment(figment.merge(env_provider()))
  }

  /// Parses a configuration from TOML text only (no environment); used by tests and tooling.
  pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
    Self::from_figment(Figment::new().merge(Toml::string(text)))
  }

  fn from_figment(figment: Figment) -> Result<Self, ConfigError> {
    figment.extract().map_err(|e| ConfigError::Invalid(Box::new(e)))
  }
}

impl DatabaseConfig {
  pub fn acquire_timeout(&self) -> Duration {
    Duration::from_secs(self.acquire_timeout_secs)
  }
}

impl Default for ServerConfig {
  fn default() -> Self {
    Self {
      listen_addr: SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 8080),
      worker_id: 0,
      partitions_ahead_months: 3,
    }
  }
}

impl Default for RoleNames {
  fn default() -> Self {
    Self {
      owner: "owner".into(),
      admin: "admin".into(),
      member: "member".into(),
      guest: "guest".into(),
    }
  }
}

impl Default for PolicyConfig {
  fn default() -> Self {
    Self {
      guests_dm_only_with_shared_conversations: true,
      invites_require_verified_email: true,
    }
  }
}

impl Default for LimitsConfig {
  fn default() -> Self {
    Self {
      max_ciphertext_bytes: 1024 * 1024,
      max_mentions_per_message: 200,
      max_group_dm_participants: 8,
      max_name_length: 100,
      max_text_length: 1000,
      default_page_size: 50,
      max_page_size: 200,
      max_directory_results: 50,
      max_key_packages_per_device: 200,
      max_key_package_bytes: 16 * 1024,
      max_key_packages_per_upload: 100,
      max_mls_payload_bytes: 1024 * 1024,
      max_welcomes_per_commit: 500,
      max_key_bag_bytes: 1024 * 1024,
    }
  }
}

impl Default for GatewayConfig {
  fn default() -> Self {
    Self {
      heartbeat_interval_secs: 30,
      heartbeat_timeout_secs: 90,
      resume_window_secs: 120,
      resume_buffer_events: 1000,
      identify_timeout_secs: 10,
      connection_max_age_secs: 3600,
      presence_offline_grace_secs: 5,
      max_frame_bytes: 64 * 1024,
    }
  }
}

impl Default for RendezvousConfig {
  fn default() -> Self {
    Self {
      ttl_secs: 300,
      max_frame_bytes: 16 * 1024,
      max_total_bytes: 256 * 1024,
      max_pending_frames: 8,
      max_per_user: 5,
      join_timeout_secs: 10,
    }
  }
}

impl Default for LoggingConfig {
  fn default() -> Self {
    Self {
      filter: "info".into(),
      format: LogFormat::Pretty,
    }
  }
}

fn env_provider() -> Env {
  Env::prefixed(ENV_PREFIX).ignore(&["config"]).split(ENV_NESTING)
}

fn default_region() -> String {
  "us-east-1".into()
}

fn default_true() -> bool {
  true
}

fn default_presign_expiry_secs() -> u64 {
  900
}

fn default_max_attachment_bytes() -> u64 {
  100 * 1024 * 1024
}

fn default_max_connections() -> u32 {
  10
}

fn default_acquire_timeout_secs() -> u64 {
  5
}

fn default_client_id() -> String {
  "teamcraft".into()
}

fn default_server_client() -> String {
  "teamcraft-server".into()
}

fn default_algorithms() -> Vec<String> {
  vec!["RS256".into(), "ES256".into(), "EdDSA".into()]
}

fn default_leeway_secs() -> u64 {
  30
}

fn default_jwks_cache_ttl_secs() -> u64 {
  3600
}

fn default_jwks_min_refresh_interval_secs() -> u64 {
  30
}

fn default_http_timeout_secs() -> u64 {
  10
}

#[cfg(test)]
mod tests {
  use std::net::SocketAddr;

  use secrecy::ExposeSecret;

  use super::*;

  const MINIMAL: &str = r#"
    [database]
    url = "postgres://u:p@localhost/db"
    [oidc]
    issuer = "https://auth.example.com/realms/company"
  "#;

  #[test]
  fn minimal_config_gets_documented_defaults() {
    let config = Config::from_toml(MINIMAL).unwrap();
    assert_eq!(
      config.server.listen_addr,
      "[::]:8080".parse::<SocketAddr>().unwrap()
    );
    assert_eq!(config.oidc.client_id, "teamcraft");
    assert_eq!(config.oidc.roles.guest, "guest");
    assert_eq!(config.database.max_connections, 10);
    assert_eq!(config.logging.format, LogFormat::Pretty);
    assert!(config.oidc.maybe_jwks_uri.is_none());
  }

  #[test]
  fn example_file_parses_and_matches_the_defaults() {
    let text = include_str!("../../../teamcraft.example.toml");
    let config = Config::from_toml(text).unwrap();
    assert_eq!(config.limits.max_page_size, LimitsConfig::default().max_page_size);
    assert_eq!(
      config.gateway.resume_buffer_events,
      GatewayConfig::default().resume_buffer_events
    );
    assert_eq!(
      config.gateway.max_frame_bytes,
      GatewayConfig::default().max_frame_bytes
    );
    assert_eq!(
      config.policy.guests_dm_only_with_shared_conversations,
      PolicyConfig::default().guests_dm_only_with_shared_conversations
    );
    assert_eq!(
      config.rendezvous.max_total_bytes,
      RendezvousConfig::default().max_total_bytes
    );
    assert_eq!(config.server.listen_addr, ServerConfig::default().listen_addr);
  }

  #[test]
  fn listen_addr_accepts_both_families() {
    let v6 = format!("{MINIMAL}\n[server]\nlisten_addr = \"[::1]:9000\"\n");
    assert!(Config::from_toml(&v6).unwrap().server.listen_addr.is_ipv6());
    let v4 = format!("{MINIMAL}\n[server]\nlisten_addr = \"127.0.0.1:9000\"\n");
    assert!(Config::from_toml(&v4).unwrap().server.listen_addr.is_ipv4());
  }

  #[test]
  fn role_names_and_client_are_configurable() {
    let text = format!("{MINIMAL}\n[oidc.roles]\nguest = \"external\"\n");
    let config = Config::from_toml(&text).unwrap();
    assert_eq!(config.oidc.roles.guest, "external");
    assert_eq!(config.oidc.roles.owner, "owner");
  }

  #[test]
  fn missing_required_values_are_rejected() {
    assert!(Config::from_toml("[oidc]\nissuer = \"https://auth.example.com\"").is_err());
    assert!(Config::from_toml("[database]\nurl = \"postgres://x\"").is_err());
  }

  #[test]
  fn unknown_keys_are_rejected() {
    let text = format!("{MINIMAL}\n[server]\nlisten_adress = \"0.0.0.0:1\"\n");
    assert!(Config::from_toml(&text).is_err());
  }

  #[test]
  #[allow(clippy::result_large_err)] // figment::Jail closure signature is fixed by the library
  fn environment_overrides_file_values() {
    figment::Jail::expect_with(|jail| {
      jail.create_file("teamcraft.toml", MINIMAL)?;
      jail.set_env("TEAMCRAFT_OIDC__ISSUER", "https://other.example.com/realms/x");
      jail.set_env("TEAMCRAFT_OIDC__ROLES__GUEST", "external");
      jail.set_env("TEAMCRAFT_SERVER__WORKER_ID", "7");
      jail.set_env("TEAMCRAFT_CONFIG", "ignored.toml");

      let config = Config::load(Some(Path::new("teamcraft.toml"))).map_err(|e| e.to_string())?;
      assert_eq!(config.oidc.issuer, "https://other.example.com/realms/x");
      assert_eq!(config.oidc.roles.guest, "external");
      assert_eq!(config.server.worker_id, 7);
      assert_eq!(config.database.url.expose_secret(), "postgres://u:p@localhost/db");
      Ok(())
    });
  }

  #[test]
  fn missing_config_file_is_an_error() {
    assert!(matches!(
      Config::load(Some(Path::new("/nonexistent/teamcraft.toml"))),
      Err(ConfigError::MissingFile(_))
    ));
  }
}
