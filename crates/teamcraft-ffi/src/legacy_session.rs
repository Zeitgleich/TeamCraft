//! The sign-out of the session from builds before the server entry (#224): the checks of
//! `teamcraft-client-api` `legacy_session`, so Android and iOS decide exactly like the desktop
//! which identity provider may receive the old refresh token and when the old session is deleted.
//! The apps fetch the discovery document and send the requests themselves.

use teamcraft_client_api::legacy_session::{
  self, LegacyCleanupStep, LegacyProvider, LegacySessionError, RevocationKind,
};
use teamcraft_client_api::server_entry::HttpPolicy;

/// Why the old refresh token is not sent anywhere. Flat: the app logs it and deletes the session
/// without the revocation.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum LegacySignOutError {
  #[error("the discovery document is not usable: {0}")]
  InvalidDiscovery(String),
  #[error("the identity provider is not the old session's")]
  OtherProvider,
  #[error("the identity provider of the old session is unknown")]
  UnknownProvider,
  #[error("the identity provider offers no way to revoke a token")]
  NoRevocationEndpoint,
}

/// What is known about the old session's identity provider.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LegacyProviderInfo {
  /// Android kept it (in the AppAuth state); iOS didn't.
  pub maybe_issuer: Option<String>,
  pub maybe_token_endpoint: Option<String>,
  pub client_id: String,
}

/// A form `POST` that revokes the old refresh token.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct LegacyRevocationRequest {
  pub url: String,
  /// RFC 7009 revocation; `false`: the end-session endpoint.
  pub is_rfc7009: bool,
  /// Form fields (name, value) in order; they contain the refresh token, never log them.
  pub form: Vec<LegacyFormField>,
}

#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct LegacyFormField {
  pub name: String,
  pub value: String,
}

/// What to do with the old session on this start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LegacyCleanupAction {
  /// Keep it: the user may still enter its server.
  WaitForServer,
  /// The active server is its server: unregister push with the old session (open its store with
  /// the old key and call `unregister_push`), revoke the refresh token, delete everything.
  UnregisterPushThenRevoke,
  /// Revoke the refresh token if possible, then delete everything.
  RevokeAndDelete,
}

/// Where the discovery document of `issuer` is.
#[uniffi::export]
pub fn legacy_discovery_url(issuer: String) -> String {
  legacy_session::discovery_url(&issuer)
}

/// The request that revokes `refresh_token`, from the discovery document fetched for `issuer`.
#[uniffi::export]
pub fn legacy_token_revocation(
  provider: LegacyProviderInfo,
  issuer: String,
  discovery_json: Vec<u8>,
  refresh_token: String,
  allow_insecure_http: bool,
) -> Result<LegacyRevocationRequest, LegacySignOutError> {
  let revocation = legacy_session::token_revocation(
    &provider.into(),
    &issuer,
    &discovery_json,
    &refresh_token,
    policy(allow_insecure_http),
  )?;
  Ok(LegacyRevocationRequest {
    url: revocation.url,
    is_rfc7009: revocation.kind == RevocationKind::Rfc7009,
    form: revocation
      .form
      .into_iter()
      .map(|(name, value)| LegacyFormField { name, value })
      .collect(),
  })
}

/// Whether the server the user entered (its profile's issuer and client, and the discovery
/// document fetched for that issuer) is the old session's.
#[uniffi::export]
pub fn is_legacy_provider(
  provider: LegacyProviderInfo,
  issuer: String,
  client_id: String,
  discovery_json: Vec<u8>,
  allow_insecure_http: bool,
) -> bool {
  legacy_session::is_legacy_provider(
    &provider.into(),
    &issuer,
    &client_id,
    &discovery_json,
    policy(allow_insecure_http),
  )
}

/// What to do with the old session, moved aside at `moved_at_ms`, on a start at `now_ms`.
#[uniffi::export]
pub fn legacy_cleanup_action(now_ms: i64, moved_at_ms: i64, is_server_matching: bool) -> LegacyCleanupAction {
  match legacy_session::legacy_cleanup_step(now_ms, moved_at_ms, is_server_matching) {
    LegacyCleanupStep::WaitForServer => LegacyCleanupAction::WaitForServer,
    LegacyCleanupStep::UnregisterPushThenRevoke => LegacyCleanupAction::UnregisterPushThenRevoke,
    LegacyCleanupStep::RevokeAndDelete => LegacyCleanupAction::RevokeAndDelete,
  }
}

impl std::fmt::Debug for LegacyRevocationRequest {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    formatter
      .debug_struct("LegacyRevocationRequest")
      .field("url", &self.url)
      .field("is_rfc7009", &self.is_rfc7009)
      .field("form", &"[redacted]")
      .finish()
  }
}

impl std::fmt::Debug for LegacyFormField {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    formatter
      .debug_struct("LegacyFormField")
      .field("name", &self.name)
      .field("value", &"[redacted]")
      .finish()
  }
}

impl From<LegacyProviderInfo> for LegacyProvider {
  fn from(info: LegacyProviderInfo) -> Self {
    Self {
      maybe_issuer: info.maybe_issuer,
      maybe_token_endpoint: info.maybe_token_endpoint,
      client_id: info.client_id,
    }
  }
}

impl From<LegacySessionError> for LegacySignOutError {
  fn from(error: LegacySessionError) -> Self {
    match error {
      LegacySessionError::InvalidDiscovery(reason) => Self::InvalidDiscovery(reason),
      LegacySessionError::Discovery(error) => Self::InvalidDiscovery(error.to_string()),
      LegacySessionError::OtherProvider => Self::OtherProvider,
      LegacySessionError::UnknownProvider => Self::UnknownProvider,
      LegacySessionError::NoRevocationEndpoint => Self::NoRevocationEndpoint,
    }
  }
}

fn policy(allow_insecure_http: bool) -> HttpPolicy {
  if allow_insecure_http {
    HttpPolicy::Allowed
  } else {
    HttpPolicy::LoopbackOnly
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const ISSUER: &str = "https://id.example.com/realms/teamcraft";

  #[test]
  fn the_revocation_goes_to_the_old_provider_only() {
    let document = br#"{"issuer":"https://id.example.com/realms/teamcraft","token_endpoint":"https://id.example.com/realms/teamcraft/token","revocation_endpoint":"https://id.example.com/realms/teamcraft/revoke"}"#;
    let request = legacy_token_revocation(
      provider(),
      ISSUER.into(),
      document.to_vec(),
      "secret".into(),
      false,
    )
    .unwrap();
    assert!(request.is_rfc7009);
    assert_eq!(request.form[0].value, "secret");
    assert!(!format!("{request:?}").contains("secret"));
    assert!(is_legacy_provider(
      provider(),
      ISSUER.into(),
      "teamcraft".into(),
      document.to_vec(),
      false
    ));

    // A server with another provider never sees the old token.
    let other =
      br#"{"issuer":"https://login.example.org","revocation_endpoint":"https://login.example.org/revoke"}"#;
    assert!(matches!(
      legacy_token_revocation(
        provider(),
        "https://login.example.org".into(),
        other.to_vec(),
        "secret".into(),
        false
      ),
      Err(LegacySignOutError::OtherProvider)
    ));
    assert!(!is_legacy_provider(
      provider(),
      "https://login.example.org".into(),
      "teamcraft".into(),
      other.to_vec(),
      false
    ));
  }

  #[test]
  fn the_cleanup_waits_within_the_bound() {
    assert_eq!(
      legacy_cleanup_action(10, 0, false),
      LegacyCleanupAction::WaitForServer
    );
    assert_eq!(
      legacy_cleanup_action(legacy_session::LEGACY_SERVER_WAIT_MS, 0, false),
      LegacyCleanupAction::RevokeAndDelete
    );
    assert_eq!(
      legacy_cleanup_action(10, 0, true),
      LegacyCleanupAction::UnregisterPushThenRevoke
    );
  }

  fn provider() -> LegacyProviderInfo {
    LegacyProviderInfo {
      maybe_issuer: Some(ISSUER.into()),
      maybe_token_endpoint: None,
      client_id: "teamcraft".into(),
    }
  }
}
