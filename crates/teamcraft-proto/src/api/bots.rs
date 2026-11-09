use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::{Host, Url};
use utoipa::ToSchema;

use crate::{UnixMillis, UserId};

/// Longest bridge public URL, in characters.
pub const MAX_BRIDGE_URL_CHARS: usize = 512;

/// A bot account (an integration bridge or another automated client).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BotResponse {
  /// The bot's user ID; add it to spaces and channels like any user.
  pub id: UserId,
  pub name: String,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
  pub created_at: UnixMillis,
  /// Revoked bots are deactivated and their token no longer works.
  pub is_revoked: bool,
  /// Paused because the person who created it was deactivated: a new token brings it back.
  #[serde(default)]
  pub is_paused: bool,
}

/// `POST /api/v1/admin/bots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateBotRequest {
  pub name: String,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
}

/// The bot with its token, shown once: only a hash is stored.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BotTokenResponse {
  pub bot: BotResponse,
  /// Bearer token (`tcb_...`) for the REST API and the gateway.
  pub token: String,
}

impl std::fmt::Debug for BotTokenResponse {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("BotTokenResponse")
      .field("bot", &self.bot)
      .field("token", &"[redacted]")
      .finish()
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListBotsResponse {
  pub bots: Vec<BotResponse>,
}

/// `GET /api/v1/conversations/{conversation_id}/bridge-address`: where producers reach the
/// integration bridge from outside, for the channel's "Connect a source" section. Only people who
/// may configure the channel's integrations get it; it holds this one setting and nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BridgeAddressResponse {
  /// The admin's "bridge public URL" without a trailing slash; absent when none is set.
  #[serde(rename = "public_url", default, skip_serializing_if = "Option::is_none")]
  pub maybe_public_url: Option<String>,
}

/// Why a text is no bridge public URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BridgeUrlError {
  #[error("the address is longer than {MAX_BRIDGE_URL_CHARS} characters")]
  TooLong,
  #[error("not a web address")]
  NotAUrl,
  #[error("the address must use https; http only for localhost")]
  Scheme,
  #[error("the address must not contain a user name or password")]
  Credentials,
  #[error("the address must not contain a query or a fragment")]
  QueryOrFragment,
}

/// Checks a bridge public URL and returns it as stored: trimmed, without trailing slashes. It is
/// the base of `/hooks/<token>` and `/ingest/<token>`, so it is an `https` address (`http` only
/// for a loopback host, where the bridge's own default listener answers), with no credentials, no
/// query and no fragment. A path is allowed, for a proxy that mounts the bridge under one.
/// Server and apps check the same way.
pub fn check_bridge_public_url(text: &str) -> Result<String, BridgeUrlError> {
  let text = text.trim();
  if text.chars().count() > MAX_BRIDGE_URL_CHARS {
    return Err(BridgeUrlError::TooLong);
  }
  if text.is_empty() || text.chars().any(|c| c.is_whitespace() || c.is_control()) {
    return Err(BridgeUrlError::NotAUrl);
  }
  let url = Url::parse(text).map_err(|_| BridgeUrlError::NotAUrl)?;
  let Some(host) = url.host() else {
    return Err(BridgeUrlError::NotAUrl);
  };
  if !url.username().is_empty() || url.password().is_some() {
    return Err(BridgeUrlError::Credentials);
  }
  if url.query().is_some() || url.fragment().is_some() || text.contains(['?', '#']) {
    return Err(BridgeUrlError::QueryOrFragment);
  }
  match url.scheme() {
    "https" => {},
    "http" if is_loopback(&host) => {},
    _ => return Err(BridgeUrlError::Scheme),
  }
  Ok(text.trim_end_matches('/').to_owned())
}

fn is_loopback(host: &Host<&str>) -> bool {
  match host {
    Host::Domain(name) => {
      let name = name.trim_end_matches('.').to_ascii_lowercase();
      name == "localhost" || name.ends_with(".localhost")
    },
    Host::Ipv4(ip) => ip.is_loopback(),
    Host::Ipv6(ip) => ip.is_loopback(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_https_address_is_kept_without_trailing_slashes() {
    assert_eq!(
      check_bridge_public_url(" https://bridge.example.com/ "),
      Ok("https://bridge.example.com".to_owned())
    );
    assert_eq!(
      check_bridge_public_url("https://example.com:8443/bridge//"),
      Ok("https://example.com:8443/bridge".to_owned())
    );
  }

  #[test]
  fn http_is_for_loopback_only() {
    for ok in [
      "http://127.0.0.1:8088",
      "http://localhost:8088",
      "http://[::1]:8088",
      "http://bridge.localhost",
    ] {
      assert!(check_bridge_public_url(ok).is_ok(), "{ok}");
    }
    for bad in [
      "http://bridge.example.com",
      "http://192.168.1.5:8088",
      "http://localhost.example.com",
    ] {
      assert_eq!(check_bridge_public_url(bad), Err(BridgeUrlError::Scheme), "{bad}");
    }
    assert_eq!(
      check_bridge_public_url("ftp://bridge.example.com"),
      Err(BridgeUrlError::Scheme)
    );
  }

  #[test]
  fn credentials_queries_and_fragments_are_refused() {
    for bad in ["https://user:pw@example.com", "https://user@example.com"] {
      assert_eq!(
        check_bridge_public_url(bad),
        Err(BridgeUrlError::Credentials),
        "{bad}"
      );
    }
    for bad in [
      "https://example.com/#top",
      "https://example.com#",
      "https://example.com/?a=1",
      "https://example.com?",
    ] {
      assert_eq!(
        check_bridge_public_url(bad),
        Err(BridgeUrlError::QueryOrFragment),
        "{bad}"
      );
    }
  }

  #[test]
  fn text_that_is_no_address_is_refused() {
    for bad in [
      "",
      "  ",
      "example.com",
      "https://",
      "https://exa mple.com",
      "javascript:alert(1)",
    ] {
      assert!(check_bridge_public_url(bad).is_err(), "{bad:?}");
    }
    let long = format!("https://example.com/{}", "a".repeat(MAX_BRIDGE_URL_CHARS));
    assert_eq!(check_bridge_public_url(&long), Err(BridgeUrlError::TooLong));
  }
}
