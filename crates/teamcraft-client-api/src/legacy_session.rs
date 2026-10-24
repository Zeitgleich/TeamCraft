//! Signing out the session of builds from before the server entry (#222, #223, #224). Those builds
//! had one server built in and kept a single session and store; the server entry keys everything by
//! server, so that state is orphaned. Before it is deleted, its refresh token is revoked at the
//! identity provider and, on the phones, its push registration removed, so nothing stays valid
//! that no app can use any more.
//!
//! No I/O, like [`crate::server_entry`]: the apps fetch the discovery document and send the
//! requests; this module decides what may be sent where. A refresh token only ever goes to an
//! endpoint of the identity provider it came from.

use serde::Deserialize;
use url::Url;

use crate::server_entry::{DiscoveryError, HttpPolicy, check_discovery};

/// How long a phone keeps the old session for the push unregistration while it waits for the
/// user to enter the server it belonged to. Afterwards the token is revoked (if its provider is
/// known) and everything deleted without it: the old device's push registration then stays until
/// an admin removes the device.
pub const LEGACY_SERVER_WAIT_MS: i64 = 14 * 24 * 60 * 60 * 1000;

/// What is known about the identity provider of an old session: its issuer (Android, desktop) or
/// only its token endpoint (iOS kept no issuer), and the client it was issued to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyProvider {
  pub maybe_issuer: Option<String>,
  pub maybe_token_endpoint: Option<String>,
  pub client_id: String,
}

/// A request that revokes the old refresh token: a form `POST` to `url`.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenRevocation {
  pub url: String,
  pub kind: RevocationKind,
  /// Form fields; they contain the refresh token.
  pub form: Vec<(String, String)>,
}

/// Which endpoint the revocation uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevocationKind {
  /// OAuth 2.0 Token Revocation (RFC 7009), preferred.
  Rfc7009,
  /// The OIDC end-session endpoint with the refresh token (Keycloak ends the session with it), the
  /// same request the desktop's sign-out sends.
  EndSession,
}

/// What a phone does with the old session on this start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyCleanupStep {
  /// Keep it: the user may still enter the server it belonged to.
  WaitForServer,
  /// The active server is the old session's: unregister push with the old session, revoke the
  /// refresh token, then delete.
  UnregisterPushThenRevoke,
  /// No matching server within [`LEGACY_SERVER_WAIT_MS`]: revoke if the provider is known, then
  /// delete.
  RevokeAndDelete,
}

/// Why the old session can't be revoked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LegacySessionError {
  #[error("the discovery document can't be read: {0}")]
  InvalidDiscovery(String),
  #[error(transparent)]
  Discovery(#[from] DiscoveryError),
  /// The discovery document is not the old session's provider.
  #[error("the identity provider is not the one of the old session")]
  OtherProvider,
  /// Neither issuer nor token endpoint is known: the provider can't be identified.
  #[error("the identity provider of the old session is unknown")]
  UnknownProvider,
  #[error("the identity provider offers no way to revoke a token")]
  NoRevocationEndpoint,
}

/// The parts of a discovery document (OIDC Discovery 1.0, RFC 8414) the revocation needs.
#[derive(Deserialize)]
struct DiscoveryDocument {
  issuer: String,
  #[serde(default)]
  token_endpoint: Option<String>,
  #[serde(default)]
  revocation_endpoint: Option<String>,
  #[serde(default)]
  end_session_endpoint: Option<String>,
}

/// Where the discovery document of `issuer` is.
pub fn discovery_url(issuer: &str) -> String {
  format!(
    "{}/.well-known/openid-configuration",
    issuer.trim_end_matches('/')
  )
}

/// The request that revokes `refresh_token` of `provider`, from the discovery document the app
/// fetched from [`discovery_url`] of `issuer` (the old session's, or the server's the user
/// entered). Refused unless the document is that issuer's, names only safe endpoints and is the old
/// session's provider: its token endpoint, when known, must be the one the token came from.
pub fn token_revocation(
  provider: &LegacyProvider,
  issuer: &str,
  discovery_json: &[u8],
  refresh_token: &str,
  policy: HttpPolicy,
) -> Result<TokenRevocation, LegacySessionError> {
  let document = belonging_discovery(provider, issuer, discovery_json, policy)?;
  if let Some(url) = document.revocation_endpoint {
    return Ok(TokenRevocation {
      url,
      kind: RevocationKind::Rfc7009,
      form: form(&[
        ("token", refresh_token),
        ("token_type_hint", "refresh_token"),
        ("client_id", &provider.client_id),
      ]),
    });
  }
  let url = document
    .end_session_endpoint
    .ok_or(LegacySessionError::NoRevocationEndpoint)?;
  Ok(TokenRevocation {
    url,
    kind: RevocationKind::EndSession,
    form: form(&[
      ("client_id", &provider.client_id),
      ("refresh_token", refresh_token),
    ]),
  })
}

/// Whether the server the user entered (its profile's issuer and client, and the discovery
/// document fetched for that issuer) is the old session's: then its push registration lives there.
pub fn is_legacy_provider(
  provider: &LegacyProvider,
  issuer: &str,
  client_id: &str,
  discovery_json: &[u8],
  policy: HttpPolicy,
) -> bool {
  provider.client_id == client_id && belonging_discovery(provider, issuer, discovery_json, policy).is_ok()
}

/// What to do with the old session, moved aside at `moved_at_ms`, on a start at `now_ms`.
/// `is_server_matching`: the active server is the old session's ([`is_legacy_provider`]).
pub fn legacy_cleanup_step(now_ms: i64, moved_at_ms: i64, is_server_matching: bool) -> LegacyCleanupStep {
  if is_server_matching {
    return LegacyCleanupStep::UnregisterPushThenRevoke;
  }
  // A clock set back makes the wait look negative: keep waiting, the bound still holds forward.
  if now_ms.saturating_sub(moved_at_ms) >= LEGACY_SERVER_WAIT_MS {
    LegacyCleanupStep::RevokeAndDelete
  } else {
    LegacyCleanupStep::WaitForServer
  }
}

impl std::fmt::Debug for TokenRevocation {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    // The form carries the refresh token.
    formatter
      .debug_struct("TokenRevocation")
      .field("url", &self.url)
      .field("kind", &self.kind)
      .field("form", &"[redacted]")
      .finish()
  }
}

/// The discovery document, if it is `issuer`'s and the old session's provider.
fn belonging_discovery(
  provider: &LegacyProvider,
  issuer: &str,
  discovery_json: &[u8],
  policy: HttpPolicy,
) -> Result<DiscoveryDocument, LegacySessionError> {
  let document: DiscoveryDocument = serde_json::from_slice(discovery_json)
    .map_err(|error| LegacySessionError::InvalidDiscovery(error.to_string()))?;
  let endpoints: Vec<&str> = [
    document.token_endpoint.as_deref(),
    document.revocation_endpoint.as_deref(),
    document.end_session_endpoint.as_deref(),
  ]
  .into_iter()
  .flatten()
  .collect();
  check_discovery(issuer, &document.issuer, &endpoints, policy)?;
  let is_same_issuer = provider
    .maybe_issuer
    .as_deref()
    .map(|legacy| same_url(legacy, issuer));
  let is_same_token_endpoint = provider.maybe_token_endpoint.as_deref().map(|legacy| {
    document
      .token_endpoint
      .as_deref()
      .is_some_and(|discovered| same_url(legacy, discovered))
  });
  match (is_same_issuer, is_same_token_endpoint) {
    (None, None) => Err(LegacySessionError::UnknownProvider),
    // Every piece that is known must agree.
    (Some(false), _) | (_, Some(false)) => Err(LegacySessionError::OtherProvider),
    _ => Ok(document),
  }
}

/// Whether two URLs are the same, ignoring a trailing slash and the case of scheme and host.
fn same_url(a: &str, b: &str) -> bool {
  let normalized = |raw: &str| {
    Url::parse(raw)
      .ok()
      .map(|url| url.as_str().trim_end_matches('/').to_owned())
  };
  match (normalized(a), normalized(b)) {
    (Some(a), Some(b)) => a == b,
    _ => false,
  }
}

fn form(fields: &[(&str, &str)]) -> Vec<(String, String)> {
  fields
    .iter()
    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  const ISSUER: &str = "https://id.example.com/realms/teamcraft";
  const TOKEN_ENDPOINT: &str = "https://id.example.com/realms/teamcraft/protocol/openid-connect/token";
  const REVOKE: &str = "https://id.example.com/realms/teamcraft/protocol/openid-connect/revoke";
  const LOGOUT: &str = "https://id.example.com/realms/teamcraft/protocol/openid-connect/logout";

  mod revocation {
    use super::*;

    #[test]
    fn rfc_7009_is_preferred() {
      let revocation = token_revocation(
        &by_issuer(),
        ISSUER,
        &discovery(ISSUER, Some(REVOKE), Some(LOGOUT)),
        "refresh-1",
        HttpPolicy::LoopbackOnly,
      )
      .unwrap();
      assert_eq!(revocation.url, REVOKE);
      assert_eq!(revocation.kind, RevocationKind::Rfc7009);
      assert_eq!(
        revocation.form,
        form(&[
          ("token", "refresh-1"),
          ("token_type_hint", "refresh_token"),
          ("client_id", "teamcraft"),
        ])
      );
      assert!(!format!("{revocation:?}").contains("refresh-1"), "never logged");
    }

    #[test]
    fn the_end_session_endpoint_is_the_fallback() {
      let revocation = token_revocation(
        &by_issuer(),
        ISSUER,
        &discovery(ISSUER, None, Some(LOGOUT)),
        "refresh-1",
        HttpPolicy::LoopbackOnly,
      )
      .unwrap();
      assert_eq!(
        (revocation.url.as_str(), revocation.kind),
        (LOGOUT, RevocationKind::EndSession)
      );
      let refused = token_revocation(
        &by_issuer(),
        ISSUER,
        &discovery(ISSUER, None, None),
        "refresh-1",
        HttpPolicy::LoopbackOnly,
      );
      assert_eq!(refused.unwrap_err(), LegacySessionError::NoRevocationEndpoint);
    }

    #[test]
    fn a_token_endpoint_alone_identifies_the_provider() {
      // iOS kept no issuer: the server's provider must have the token endpoint the token came from.
      let provider = LegacyProvider {
        maybe_issuer: None,
        maybe_token_endpoint: Some(TOKEN_ENDPOINT.to_owned()),
        client_id: "teamcraft".to_owned(),
      };
      let document = discovery(ISSUER, Some(REVOKE), None);
      assert!(token_revocation(&provider, ISSUER, &document, "t", HttpPolicy::LoopbackOnly).is_ok());
      let unknown = LegacyProvider {
        maybe_token_endpoint: None,
        ..provider
      };
      assert_eq!(
        token_revocation(&unknown, ISSUER, &document, "t", HttpPolicy::LoopbackOnly).unwrap_err(),
        LegacySessionError::UnknownProvider
      );
    }
  }

  mod hostile {
    use super::*;

    #[test]
    fn a_document_of_another_issuer_gets_no_token() {
      let forged = discovery(
        "https://evil.example.net",
        Some("https://evil.example.net/revoke"),
        None,
      );
      assert_eq!(
        token_revocation(&by_issuer(), ISSUER, &forged, "t", HttpPolicy::LoopbackOnly).unwrap_err(),
        LegacySessionError::Discovery(DiscoveryError::IssuerMismatch)
      );
    }

    #[test]
    fn another_provider_of_the_entered_server_gets_no_token() {
      // The user entered a server with another identity provider: the old token stays away from it.
      let other = "https://login.example.org";
      let document = discovery(other, Some("https://login.example.org/revoke"), None);
      assert_eq!(
        token_revocation(&by_issuer(), other, &document, "t", HttpPolicy::LoopbackOnly).unwrap_err(),
        LegacySessionError::OtherProvider
      );
      let provider = LegacyProvider {
        maybe_token_endpoint: Some(TOKEN_ENDPOINT.to_owned()),
        ..by_issuer()
      };
      let moved_token_endpoint = br#"{"issuer":"https://id.example.com/realms/teamcraft","token_endpoint":"https://id.example.com/other/token","revocation_endpoint":"https://id.example.com/revoke"}"#;
      assert_eq!(
        token_revocation(
          &provider,
          ISSUER,
          moved_token_endpoint,
          "t",
          HttpPolicy::LoopbackOnly
        )
        .unwrap_err(),
        LegacySessionError::OtherProvider,
        "every known piece must agree"
      );
    }

    #[test]
    fn plain_http_and_garbage_are_refused() {
      let unsafe_endpoint = discovery(ISSUER, Some("http://id.example.com/revoke"), None);
      assert_eq!(
        token_revocation(
          &by_issuer(),
          ISSUER,
          &unsafe_endpoint,
          "t",
          HttpPolicy::LoopbackOnly
        )
        .unwrap_err(),
        LegacySessionError::Discovery(DiscoveryError::UnsafeEndpoint)
      );
      for garbage in [&b""[..], b"<html>captive portal</html>", b"{\"issuer\": 5}"] {
        assert!(matches!(
          token_revocation(&by_issuer(), ISSUER, garbage, "t", HttpPolicy::LoopbackOnly),
          Err(LegacySessionError::InvalidDiscovery(_))
        ));
      }
    }
  }

  mod matching_and_steps {
    use super::*;

    #[test]
    fn the_server_matches_by_provider_and_client() {
      let document = discovery(ISSUER, Some(REVOKE), None);
      let policy = HttpPolicy::LoopbackOnly;
      assert!(is_legacy_provider(
        &by_issuer(),
        ISSUER,
        "teamcraft",
        &document,
        policy
      ));
      assert!(is_legacy_provider(
        &by_issuer(),
        &format!("{ISSUER}/"),
        "teamcraft",
        &discovery(&format!("{ISSUER}/"), Some(REVOKE), None),
        policy
      ));
      assert!(!is_legacy_provider(
        &by_issuer(),
        ISSUER,
        "other-client",
        &document,
        policy
      ));
    }

    #[test]
    fn the_old_session_waits_for_its_server_within_the_bound() {
      let moved = 1_000;
      assert_eq!(
        legacy_cleanup_step(moved + 5, moved, false),
        LegacyCleanupStep::WaitForServer
      );
      assert_eq!(
        legacy_cleanup_step(moved + 5, moved, true),
        LegacyCleanupStep::UnregisterPushThenRevoke
      );
      assert_eq!(
        legacy_cleanup_step(moved + LEGACY_SERVER_WAIT_MS, moved, false),
        LegacyCleanupStep::RevokeAndDelete
      );
      assert_eq!(
        legacy_cleanup_step(moved - 10_000, moved, false),
        LegacyCleanupStep::WaitForServer,
        "a clock set back"
      );
    }

    #[test]
    fn the_discovery_url_has_one_slash() {
      assert_eq!(
        discovery_url("https://id.example.com/realms/a/"),
        "https://id.example.com/realms/a/.well-known/openid-configuration"
      );
    }
  }

  fn by_issuer() -> LegacyProvider {
    LegacyProvider {
      maybe_issuer: Some(ISSUER.to_owned()),
      maybe_token_endpoint: None,
      client_id: "teamcraft".to_owned(),
    }
  }

  fn discovery(issuer: &str, maybe_revoke: Option<&str>, maybe_logout: Option<&str>) -> Vec<u8> {
    let mut document = serde_json::json!({ "issuer": issuer, "token_endpoint": TOKEN_ENDPOINT });
    if let Some(revoke) = maybe_revoke {
      document["revocation_endpoint"] = revoke.into();
    }
    if let Some(logout) = maybe_logout {
      document["end_session_endpoint"] = logout.into();
    }
    serde_json::to_vec(&document).unwrap()
  }
}
