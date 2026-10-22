use crate::settings::load_string;

const KEY_SERVER_URL: &str = "teamcraft.server_url";
const KEY_ISSUER: &str = "teamcraft.oidc_issuer";
const KEY_CLIENT_ID: &str = "teamcraft.oidc_client_id";

/// Where the server and the identity provider live. Defaults match the dev stack in the README.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppConfig {
  /// Base URL of `teamcraft-server`; empty means "same origin" (the web app served by the server or
  /// the dev proxy of Trunk).
  pub server_url: String,
  pub issuer: String,
  pub client_id: String,
}

impl AppConfig {
  pub fn dev_defaults() -> Self {
    Self {
      server_url: String::new(),
      issuer: "http://127.0.0.1:8080/realms/teamcraft-dev".to_owned(),
      client_id: "teamcraft".to_owned(),
    }
  }

  /// Defaults, overridden per browser through `localStorage` keys `teamcraft.server_url`,
  /// `teamcraft.oidc_issuer` and `teamcraft.oidc_client_id`.
  pub fn load() -> Self {
    let defaults = Self::dev_defaults();
    Self {
      server_url: load_string(KEY_SERVER_URL).unwrap_or(defaults.server_url),
      issuer: load_string(KEY_ISSUER).unwrap_or(defaults.issuer),
      client_id: load_string(KEY_CLIENT_ID).unwrap_or(defaults.client_id),
    }
  }

  /// Full URL for an API path such as `/api/v1/me`.
  pub fn api_url(&self, path: &str) -> String {
    format!("{}{path}", self.server_url.trim_end_matches('/'))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn api_url_joins_without_double_slash() {
    let mut config = AppConfig::dev_defaults();
    assert_eq!(config.api_url("/api/v1/me"), "/api/v1/me");
    config.server_url = "http://127.0.0.1:8081/".into();
    assert_eq!(config.api_url("/api/v1/me"), "http://127.0.0.1:8081/api/v1/me");
  }
}
