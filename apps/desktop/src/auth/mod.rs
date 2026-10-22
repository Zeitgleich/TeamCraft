//! Sign-in for the desktop app: OIDC authorization code + PKCE with the system browser (RFC 8252),
//! a loopback redirect on `127.0.0.1` with a random port, and the refresh token in the OS keyring.
//!
//! The webview talks to this module only through the commands in [`commands`]; access and refresh
//! tokens never reach it. API calls go through [`AuthService::api_get`], which attaches the bearer
//! token and refreshes it when needed.

pub mod commands;
mod loopback;
mod store;

#[cfg(all(test, feature = "integration"))]
pub(crate) mod integration_tests;

use std::time::{Duration, Instant};

use oauth2::basic::{BasicClient, BasicErrorResponseType, BasicTokenResponse};
use oauth2::{
  AuthUrl, AuthorizationCode, ClientId, CsrfToken, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl,
  RefreshToken, RequestTokenError, Scope, StandardErrorResponse, TokenResponse, TokenUrl,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};

pub use store::{KeyringStore, TokenStore};

const SCOPE: &str = "openid";
/// How long the browser may take to come back.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// Refresh this long before the access token expires.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// The webview may only ask for API paths below this prefix.
const API_PREFIX: &str = "/api/v1/";

/// Where the server and the identity provider live (environment variables override the dev defaults).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopConfig {
  pub server_url: String,
  pub issuer: String,
  pub client_id: String,
}

impl DesktopConfig {
  pub fn from_env() -> Self {
    let var = |name: &str, default: &str| {
      std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned())
    };
    Self {
      server_url: var("TEAMCRAFT_SERVER_URL", "http://127.0.0.1:8081"),
      issuer: var(
        "TEAMCRAFT_OIDC_ISSUER",
        "http://127.0.0.1:8080/realms/teamcraft-dev",
      ),
      client_id: var("TEAMCRAFT_OIDC_CLIENT_ID", "teamcraft"),
    }
  }
}

/// Failure of a sign-in step. `kind` is a stable identifier the UI maps to its own messages.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
  #[error("sign-in was cancelled")]
  Cancelled,
  #[error("the identity provider refused the request: {0}")]
  Denied(String),
  #[error("network error: {0}")]
  Network(String),
  #[error("unexpected response: {0}")]
  InvalidResponse(String),
  #[error("the sign-in response did not match the request")]
  StateMismatch,
  #[error("not signed in")]
  NotSignedIn,
  #[error("not supported here: {0}")]
  Unsupported(String),
}

impl AuthError {
  pub fn kind(&self) -> &'static str {
    match self {
      Self::Cancelled => "cancelled",
      Self::Denied(_) => "denied",
      Self::Network(_) => "network",
      Self::InvalidResponse(_) => "invalid_response",
      Self::StateMismatch => "state_mismatch",
      Self::NotSignedIn => "not_signed_in",
      Self::Unsupported(_) => "unsupported",
    }
  }
}

/// Response of an authenticated API call, as handed to the webview.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ApiResponse {
  pub status: u16,
  pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Discovery {
  authorization_endpoint: String,
  token_endpoint: String,
  #[serde(default)]
  end_session_endpoint: Option<String>,
}

struct Tokens {
  access: SecretString,
  refresh: Option<SecretString>,
  expires_at: Instant,
}

#[derive(Default)]
struct Session {
  maybe_tokens: Option<Tokens>,
  maybe_discovery: Option<Discovery>,
}

pub struct AuthService<S: TokenStore> {
  config: DesktopConfig,
  http: reqwest::Client,
  store: S,
  session: Mutex<Session>,
  cancel: Notify,
}

impl<S: TokenStore> AuthService<S> {
  pub fn new(config: DesktopConfig, store: S) -> Result<Self, AuthError> {
    let http = reqwest::Client::builder()
      .timeout(HTTP_TIMEOUT)
      // A redirect would carry the bearer token to wherever a hostile answer points.
      .redirect(reqwest::redirect::Policy::none())
      .build()
      .map_err(|error| AuthError::Unsupported(error.to_string()))?;
    Ok(Self {
      config,
      http,
      store,
      session: Mutex::new(Session::default()),
      cancel: Notify::new(),
    })
  }

  /// Signs in with a stored refresh token, if there is one. `Ok(false)` means "sign in again".
  pub async fn restore(&self) -> Result<bool, AuthError> {
    let Some(refresh) = self.store.load().await? else {
      return Ok(false);
    };
    let mut session = self.session.lock().await;
    match self.refresh_with(&mut session, refresh).await {
      Ok(()) => Ok(true),
      // The provider rejected the token (expired, revoked, user disabled): forget it.
      Err(AuthError::Denied(_) | AuthError::NotSignedIn) => {
        self.store.clear().await?;
        Ok(false)
      },
      Err(other) => Err(other),
    }
  }

  /// Runs the browser login. `open_browser` receives the authorization URL.
  pub async fn login(&self, open_browser: impl FnOnce(&str) -> Result<(), String>) -> Result<(), AuthError> {
    let discovery = self.discovery().await?;
    let (listener, redirect_uri) = loopback::bind().await?;
    let client = self.client(&discovery, &redirect_uri)?;
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, state) = client
      .authorize_url(CsrfToken::new_random)
      .add_scope(Scope::new(SCOPE.to_owned()))
      .set_pkce_challenge(challenge)
      .url();
    open_browser(url.as_str())
      .map_err(|error| AuthError::Unsupported(format!("could not open the browser: {error}")))?;

    let wait = loopback::wait_for_redirect(listener, state.secret(), &redirect_uri);
    let params = tokio::select! {
      result = tokio::time::timeout(LOGIN_TIMEOUT, wait) => result.map_err(|_| AuthError::Cancelled)??,
      () = self.cancel.notified() => return Err(AuthError::Cancelled),
    };
    let code = params.into_code()?;

    let http = self.oauth_http();
    let response = client
      .exchange_code(AuthorizationCode::new(code))
      .set_pkce_verifier(PkceCodeVerifier::new(verifier.secret().clone()))
      .request_async(&http)
      .await
      .map_err(map_token_error)?;
    let mut session = self.session.lock().await;
    self.adopt(&mut session, &response).await
  }

  /// Aborts a login that waits for the browser.
  pub fn cancel_login(&self) {
    self.cancel.notify_waiters();
  }

  /// Ends the session at the provider (best effort) and forgets everything stored locally.
  pub async fn logout(&self) -> Result<(), AuthError> {
    let (in_memory, discovery) = {
      let mut session = self.session.lock().await;
      (
        session.maybe_tokens.take().and_then(|tokens| tokens.refresh),
        session.maybe_discovery.clone(),
      )
    };
    let refresh = match in_memory {
      Some(token) => Some(token),
      None => self.store.load().await?,
    };
    self.store.clear().await?;
    if let (Some(refresh), Some(endpoint)) = (refresh, discovery.and_then(|found| found.end_session_endpoint))
    {
      let form = [
        ("client_id", self.config.client_id.as_str()),
        ("refresh_token", refresh.expose_secret()),
      ];
      if let Err(error) = self.http.post(endpoint).form(&form).send().await {
        // The local session is gone either way; the provider session expires on its own.
        tracing::warn!(%error, "could not end the provider session");
      }
    }
    Ok(())
  }

  /// `GET {server}{path}` with a fresh access token.
  pub async fn api_get(&self, path: &str) -> Result<ApiResponse, AuthError> {
    validate_api_path(path)?;
    let token = self.access_token().await?;
    let url = format!("{}{path}", self.config.server_url.trim_end_matches('/'));
    let response = self
      .http
      .get(url)
      .bearer_auth(token.expose_secret())
      .header(reqwest::header::ACCEPT, "application/json")
      .send()
      .await
      .map_err(|error| AuthError::Network(error.to_string()))?;
    let status = response.status().as_u16();
    let body = response
      .text()
      .await
      .map_err(|error| AuthError::Network(error.to_string()))?;
    Ok(ApiResponse { status, body })
  }

  /// The current access token as text, refreshed when needed. For the in-process client core only:
  /// the webview never gets it.
  pub async fn bearer_token(&self) -> Result<String, AuthError> {
    Ok(self.access_token().await?.expose_secret().to_owned())
  }

  async fn access_token(&self) -> Result<SecretString, AuthError> {
    let mut session = self.session.lock().await;
    let needs_refresh = match &session.maybe_tokens {
      None => return Err(AuthError::NotSignedIn),
      Some(tokens) => tokens.expires_at.saturating_duration_since(Instant::now()) <= REFRESH_MARGIN,
    };
    if needs_refresh {
      let refresh = session
        .maybe_tokens
        .as_ref()
        .and_then(|tokens| {
          tokens
            .refresh
            .as_ref()
            .map(|token| SecretString::from(token.expose_secret().to_owned()))
        })
        .ok_or(AuthError::NotSignedIn)?;
      if let Err(error) = self.refresh_with(&mut session, refresh).await {
        if matches!(error, AuthError::Denied(_) | AuthError::NotSignedIn) {
          session.maybe_tokens = None;
        }
        return Err(error);
      }
    }
    session
      .maybe_tokens
      .as_ref()
      .map(|tokens| SecretString::from(tokens.access.expose_secret().to_owned()))
      .ok_or(AuthError::NotSignedIn)
  }

  async fn refresh_with(&self, session: &mut Session, refresh: SecretString) -> Result<(), AuthError> {
    let discovery = self.discovery_in(session).await?;
    // The redirect URI is not used for refreshing, but the client needs a valid one.
    let client = self.client(&discovery, "http://127.0.0.1/callback")?;
    let http = self.oauth_http();
    let response = client
      .exchange_refresh_token(&RefreshToken::new(refresh.expose_secret().to_owned()))
      .request_async(&http)
      .await
      .map_err(map_token_error)?;
    self.adopt(session, &response).await
  }

  /// Takes over a token response: keeps the access token in memory and persists the refresh token.
  async fn adopt(&self, session: &mut Session, response: &BasicTokenResponse) -> Result<(), AuthError> {
    let previous = session.maybe_tokens.as_ref().and_then(|tokens| {
      tokens
        .refresh
        .as_ref()
        .map(|token| token.expose_secret().to_owned())
    });
    let refresh = response
      .refresh_token()
      .map(|token| token.secret().clone())
      .or(previous);
    if let Some(refresh) = &refresh {
      self.store.save(refresh).await?;
    }
    let lifetime = response.expires_in().unwrap_or(Duration::from_secs(60));
    session.maybe_tokens = Some(Tokens {
      access: SecretString::from(response.access_token().secret().clone()),
      refresh: refresh.map(SecretString::from),
      expires_at: Instant::now() + lifetime,
    });
    Ok(())
  }

  async fn discovery(&self) -> Result<Discovery, AuthError> {
    let mut session = self.session.lock().await;
    self.discovery_in(&mut session).await
  }

  async fn discovery_in(&self, session: &mut Session) -> Result<Discovery, AuthError> {
    if let Some(found) = &session.maybe_discovery {
      return Ok(found.clone());
    }
    let url = format!(
      "{}/.well-known/openid-configuration",
      self.config.issuer.trim_end_matches('/')
    );
    let response = self
      .http
      .get(url)
      .send()
      .await
      .map_err(|error| AuthError::Network(error.to_string()))?;
    if !response.status().is_success() {
      return Err(AuthError::Network(format!(
        "discovery returned HTTP {}",
        response.status().as_u16()
      )));
    }
    let found: Discovery = response
      .json()
      .await
      .map_err(|error| AuthError::InvalidResponse(error.to_string()))?;
    session.maybe_discovery = Some(found.clone());
    Ok(found)
  }

  fn client(
    &self,
    discovery: &Discovery,
    redirect_uri: &str,
  ) -> Result<
    BasicClient<
      oauth2::EndpointSet,
      oauth2::EndpointNotSet,
      oauth2::EndpointNotSet,
      oauth2::EndpointNotSet,
      oauth2::EndpointSet,
    >,
    AuthError,
  > {
    let invalid = |error: url::ParseError| AuthError::InvalidResponse(error.to_string());
    Ok(
      BasicClient::new(ClientId::new(self.config.client_id.clone()))
        .set_auth_uri(AuthUrl::new(discovery.authorization_endpoint.clone()).map_err(invalid)?)
        .set_token_uri(TokenUrl::new(discovery.token_endpoint.clone()).map_err(invalid)?)
        .set_redirect_uri(RedirectUrl::new(redirect_uri.to_owned()).map_err(invalid)?),
    )
  }

  /// HTTP client of the `oauth2` crate on top of our `reqwest` client.
  fn oauth_http(&self) -> OAuthHttp {
    OAuthHttp(self.http.clone())
  }
}

/// Adapter that lets the `oauth2` crate send its token requests through our `reqwest` client.
struct OAuthHttp(reqwest::Client);

impl<'c> oauth2::AsyncHttpClient<'c> for OAuthHttp {
  type Error = OAuthHttpError;
  type Future = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<oauth2::HttpResponse, OAuthHttpError>> + Send + 'c>,
  >;

  fn call(&'c self, request: oauth2::HttpRequest) -> Self::Future {
    Box::pin(async move {
      let (parts, body) = request.into_parts();
      let response = self
        .0
        .request(parts.method, parts.uri.to_string())
        .headers(parts.headers)
        .body(body)
        .send()
        .await
        .map_err(|error| OAuthHttpError(error.to_string()))?;
      let mut builder = oauth2::http::Response::builder().status(response.status());
      for (name, value) in response.headers() {
        builder = builder.header(name, value);
      }
      let bytes = response
        .bytes()
        .await
        .map_err(|error| OAuthHttpError(error.to_string()))?;
      builder
        .body(bytes.to_vec())
        .map_err(|error| OAuthHttpError(error.to_string()))
    })
  }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct OAuthHttpError(String);

type TokenError = RequestTokenError<OAuthHttpError, StandardErrorResponse<BasicErrorResponseType>>;

fn map_token_error(error: TokenError) -> AuthError {
  match error {
    RequestTokenError::ServerResponse(response) => AuthError::Denied(
      response
        .error_description()
        .cloned()
        .unwrap_or_else(|| response.error().to_string()),
    ),
    RequestTokenError::Request(error) => AuthError::Network(error.to_string()),
    RequestTokenError::Parse(error, _) => AuthError::InvalidResponse(error.to_string()),
    RequestTokenError::Other(message) => AuthError::InvalidResponse(message),
  }
}

/// The webview may only reach the API of the configured server, never an arbitrary URL.
fn validate_api_path(path: &str) -> Result<(), AuthError> {
  let is_plain = path.chars().all(|ch| {
    ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.' | '?' | '=' | '&' | ',' | '%')
  });
  if path.starts_with(API_PREFIX) && is_plain && !path.contains("..") && !path.contains("//") {
    Ok(())
  } else {
    Err(AuthError::Unsupported(format!(
      "path {path:?} is not an API path"
    )))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_plain_api_paths_are_allowed() {
    for ok in [
      "/api/v1/me",
      "/api/v1/conversations/123/members",
      "/api/v1/presence?user_ids=1,2",
    ] {
      assert!(validate_api_path(ok).is_ok(), "{ok}");
    }
    let bad_paths = [
      "/healthz",
      "http://evil.example/api/v1/me",
      "/api/v1/../admin",
      "//evil.example/api/v1/x",
      "/api/v1/me#x",
      "api/v1/me",
      "/api/v1/ me",
    ];
    for bad in bad_paths {
      assert!(validate_api_path(bad).is_err(), "{bad}");
    }
  }

  #[test]
  fn error_kinds_are_stable() {
    assert_eq!(AuthError::Cancelled.kind(), "cancelled");
    assert_eq!(AuthError::Network("x".into()).kind(), "network");
    assert_eq!(AuthError::StateMismatch.kind(), "state_mismatch");
  }
}
