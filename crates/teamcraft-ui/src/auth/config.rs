use crate::settings::load_string;

const KEY_SERVER_URL: &str = "teamcraft.server_url";
const KEY_ISSUER: &str = "teamcraft.oidc_issuer";
const KEY_CLIENT_ID: &str = "teamcraft.oidc_client_id";

/// Where the server lives, and optional development overrides of how to sign in. Without
/// overrides the browser build asks the server (`GET /api/v1/server-info`), so the web client
/// served by any instance signs in to that instance's identity provider.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AppConfig {
  /// Base URL of `teamcraft-server`; empty means "same origin" (the web app served by the server or
  /// the dev proxy of Trunk).
  pub server_url: String,
  /// Development only: an issuer to use instead of the server's.
  pub maybe_issuer: Option<String>,
  /// Development only: a client ID to use instead of the server's.
  pub maybe_client_id: Option<String>,
}

impl AppConfig {
  /// The same origin and the server's sign-in settings, overridden per browser through the
  /// `localStorage` keys `teamcraft.server_url`, `teamcraft.oidc_issuer` and
  /// `teamcraft.oidc_client_id`.
  pub fn load() -> Self {
    Self {
      server_url: load_string(KEY_SERVER_URL).unwrap_or_default(),
      maybe_issuer: load_string(KEY_ISSUER).filter(|value| !value.is_empty()),
      maybe_client_id: load_string(KEY_CLIENT_ID).filter(|value| !value.is_empty()),
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
    let mut config = AppConfig::default();
    assert_eq!(config.api_url("/api/v1/me"), "/api/v1/me");
    config.server_url = "http://127.0.0.1:8081/".into();
    assert_eq!(config.api_url("/api/v1/me"), "http://127.0.0.1:8081/api/v1/me");
  }
}
