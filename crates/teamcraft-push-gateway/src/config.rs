//! Gateway configuration: an optional TOML file (`--config <path>` or
//! `TEAMCRAFT_PUSH_GATEWAY_CONFIG`) with `TEAMCRAFT_PUSH_GATEWAY__SECTION__KEY` environment
//! overrides, like the server's. `teamcraft-push-gateway.example.toml` documents every option.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Env, Format, Toml};
use secrecy::SecretString;
use serde::Deserialize;

const ENV_PREFIX: &str = "TEAMCRAFT_PUSH_GATEWAY__";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GatewayConfig {
  pub server: ServerConfig,
  pub database: DatabaseConfig,
  pub registration: RegistrationConfig,
  pub limits: LimitsConfig,
  pub apns: ApnsConfig,
  pub fcm: FcmConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
  /// The public API. Dual stack by default.
  pub bind: SocketAddr,
  /// `/healthz` and `/metrics`; keep it off the internet (the container image binds all addresses
  /// and publishes no port for it).
  pub health_bind: SocketAddr,
  /// Header with the client address, set by a reverse proxy you trust (e.g. `x-forwarded-for`).
  /// Without it the TCP peer address is used. Only registration limits depend on it.
  #[serde(rename = "client_ip_header")]
  pub maybe_client_ip_header: Option<String>,
  pub provider_timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DatabaseConfig {
  /// The SQLite file; its directory must be writable.
  pub path: PathBuf,
  /// How long a server's record (key, block state, limits) is cached, so `block` takes effect
  /// within this time.
  pub server_cache_secs: u64,
  /// How often usage counters are written to the database.
  pub flush_interval_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationMode {
  /// Any server can register itself.
  Open,
  /// Only keys added with `teamcraft-push-gateway servers allow`.
  Closed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RegistrationConfig {
  pub mode: RegistrationMode,
  /// New registrations per client address and hour (IPv6: per /64).
  pub per_address_per_hour: u32,
  /// New registrations per hour in total.
  pub global_per_hour: u32,
  pub max_servers: u32,
}

/// Defaults for every server; `servers limits` overrides them per server.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsConfig {
  pub pushes_per_second: u32,
  pub burst: u32,
  pub pushes_per_day: u64,
  pub pushes_per_token_per_minute: u32,
  pub max_body_bytes: usize,
  /// Largest sealed wake-up.
  pub max_sealed_bytes: usize,
  pub max_ttl_secs: u32,
  /// How far a request's timestamp may differ from the gateway's clock.
  pub timestamp_window_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ApnsConfig {
  /// Contents of the `.p8` key.
  #[serde(rename = "key")]
  pub maybe_key: Option<SecretString>,
  #[serde(rename = "key_path")]
  pub maybe_key_path: Option<PathBuf>,
  #[serde(rename = "key_id")]
  pub maybe_key_id: Option<String>,
  #[serde(rename = "team_id")]
  pub maybe_team_id: Option<String>,
  /// The store app's bundle ID (topic; calls use `<bundle_id>.voip`).
  #[serde(rename = "bundle_id")]
  pub maybe_bundle_id: Option<String>,
  pub use_sandbox: bool,
  /// Visible text of the generic alert the notification extension replaces. Fixed here, so a server
  /// cannot show its own text on someone's lock screen.
  pub alert_text: String,
  #[serde(rename = "base_url")]
  pub maybe_base_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FcmConfig {
  /// Contents of the service account JSON key.
  #[serde(rename = "service_account")]
  pub maybe_service_account: Option<SecretString>,
  #[serde(rename = "service_account_path")]
  pub maybe_service_account_path: Option<PathBuf>,
  #[serde(rename = "base_url")]
  pub maybe_base_url: Option<String>,
}

impl GatewayConfig {
  pub fn load(maybe_path: Option<&Path>) -> Result<Self, Box<figment::Error>> {
    let mut figment = Figment::new();
    if let Some(path) = maybe_path {
      figment = figment.merge(Toml::file_exact(path));
    }
    figment
      .merge(Env::prefixed(ENV_PREFIX).split("__"))
      .extract()
      .map_err(Box::new)
  }
}

impl Default for ServerConfig {
  fn default() -> Self {
    Self {
      bind: SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 8090)),
      health_bind: SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 8091)),
      maybe_client_ip_header: None,
      provider_timeout_secs: 10,
    }
  }
}

impl Default for DatabaseConfig {
  fn default() -> Self {
    Self {
      path: "teamcraft-push-gateway.db".into(),
      server_cache_secs: 10,
      flush_interval_secs: 10,
    }
  }
}

impl Default for RegistrationConfig {
  fn default() -> Self {
    Self {
      mode: RegistrationMode::Open,
      per_address_per_hour: 10,
      global_per_hour: 600,
      max_servers: 100_000,
    }
  }
}

impl Default for LimitsConfig {
  fn default() -> Self {
    Self {
      pushes_per_second: 50,
      burst: 200,
      pushes_per_day: 200_000,
      pushes_per_token_per_minute: 30,
      max_body_bytes: 16 * 1024,
      max_sealed_bytes: 2048,
      max_ttl_secs: 7 * 24 * 3600,
      timestamp_window_secs: 300,
    }
  }
}

impl Default for ApnsConfig {
  fn default() -> Self {
    Self {
      maybe_key: None,
      maybe_key_path: None,
      maybe_key_id: None,
      maybe_team_id: None,
      maybe_bundle_id: None,
      use_sandbox: false,
      alert_text: "New message".into(),
      maybe_base_url: None,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  // figment::Jail wants a closure returning its large error type.
  #[allow(clippy::result_large_err)]
  #[test]
  fn environment_values_survive_figment_parsing() {
    use secrecy::ExposeSecret as _;
    figment::Jail::expect_with(|jail| {
      let pem = "-----BEGIN PRIVATE KEY-----\nMIGH\n-----END PRIVATE KEY-----\n";
      let account = r#"{"type": "service_account", "project_id": "p", "private_key": "a\nb"}"#;
      jail.set_env("TEAMCRAFT_PUSH_GATEWAY__SERVER__BIND", "[::]:9001");
      jail.set_env("TEAMCRAFT_PUSH_GATEWAY__APNS__KEY", pem);
      jail.set_env("TEAMCRAFT_PUSH_GATEWAY__FCM__SERVICE_ACCOUNT", account);
      jail.set_env("TEAMCRAFT_PUSH_GATEWAY__REGISTRATION__MODE", "closed");
      let config = GatewayConfig::load(None).map_err(|e| e.to_string())?;
      assert_eq!(config.server.bind.to_string(), "[::]:9001");
      assert_eq!(config.registration.mode, RegistrationMode::Closed);
      // A trailing newline is trimmed; PEM parsers don't need it.
      assert_eq!(config.apns.maybe_key.unwrap().expose_secret(), pem.trim_end());
      assert_eq!(config.fcm.maybe_service_account.unwrap().expose_secret(), account);
      Ok(())
    });
  }

  #[test]
  fn defaults_are_usable_and_unknown_keys_are_refused() {
    // Not load(): the test above sets environment variables.
    let config = GatewayConfig::default();
    assert_eq!(config.registration.mode, RegistrationMode::Open);
    assert!(config.server.bind.is_ipv6());
    let figment = Figment::new().merge(Toml::string("[limits]\nbogus = 1"));
    assert!(figment.extract::<GatewayConfig>().is_err());
  }
}
