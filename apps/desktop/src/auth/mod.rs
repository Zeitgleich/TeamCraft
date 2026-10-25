//! Sign-in for the desktop app: OIDC authorization code + PKCE with the system browser (RFC 8252),
//! a loopback redirect on `127.0.0.1` with a random port, and the refresh token in the OS keyring.
//!
//! The webview talks to this module only through the commands in [`commands`]; access and refresh
//! tokens never reach it. API calls go through [`AuthService::api_get`], which attaches the bearer
//! token and refreshes it when needed.
//!
//! Which server and identity provider to use is not built in: the user enters the server (or the
//! device's managed configuration names it), the server's `GET /api/v1/server-info` says how to
//! sign in, and [`servers`] keeps that per account (docs/operations.md "Server address in the
//! apps"). Tokens are stored per server origin.

mod avatars;
mod callback_page;
pub mod commands;
pub mod legacy;
mod local;
mod loopback;
pub mod servers;
mod store;
#[cfg(test)]
mod test_dir;

#[cfg(all(test, feature = "integration"))]
pub(crate) mod integration_tests;

use std::sync::{PoisonError, RwLock};
use std::time::{Duration, Instant};

use base64::Engine;
use oauth2::basic::BasicErrorResponseType;
use oauth2::{
  AuthUrl, AuthorizationCode, ClientId, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RefreshToken,
  RequestTokenError, StandardErrorResponse, TokenResponse, TokenUrl,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};

use teamcraft_client_api::oidc_login::{
  LoginRequest, OidcClient, OidcTokenResponse, authorization_url, oidc_client,
};
use teamcraft_client_api::remote::ApiMethod;
use teamcraft_client_api::server_entry::{
  self, CheckedServer, HttpPolicy, ServerAnswer, ServerCheckError, ServerOrigin, ServerProfile,
  check_discovery, is_provider_endpoint,
};
use teamcraft_client_api::sign_in_look::SignInLook;

use callback_page::CallbackOutcome;
use local::{LOCAL_LOGIN_PATH, LOCAL_LOGOUT_PATH, LocalRefusal, LocalSession};
#[cfg(test)]
pub(crate) use store::MemoryStore;
pub use store::{KeyringStore, TokenStore};

/// How long the browser may take to come back.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// Refresh this long before the access token expires.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);
/// An identity provider token this close to its expiry is not handed to the core any more: a
/// request with a token that runs out on the way fails anyway (local sessions use the same margin).
const TOKEN_EXPIRY_MARGIN: Duration = Duration::from_secs(30);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// The webview may only ask for API paths below this prefix.
const API_PREFIX: &str = "/api/v1/";
/// Brand images the webview may ask for without credentials.
const BRAND_ASSET_PREFIX: &str = "/api/v1/brand/assets/";
/// The largest brand image passed to the webview.
const MAX_BRAND_ASSET_BYTES: usize = 2 * 1024 * 1024;
/// The largest `server-info` answer read; a real one is a few KiB.
const MAX_SERVER_INFO_BYTES: usize = 1024 * 1024;
/// The redirect URI the identity provider sees, up to the port picked per sign-in (RFC 8252).
pub const REDIRECT_URI_PATTERN: &str = "http://127.0.0.1/callback";
/// Release builds take `https` servers only (plain `http` on this computer); debug builds anything.
pub const HTTP_POLICY: HttpPolicy = if cfg!(debug_assertions) {
  HttpPolicy::Allowed
} else {
  HttpPolicy::LoopbackOnly
};

/// The active server and how to sign in to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopConfig {
  /// The server's origin: what tokens and local data are keyed by.
  pub server_url: String,
  pub issuer: String,
  pub client_id: String,
  pub scopes: Vec<String>,
}

impl DesktopConfig {
  pub fn from_profile(profile: &ServerProfile) -> Self {
    Self {
      server_url: profile.origin.as_str().to_owned(),
      issuer: profile.issuer.clone(),
      client_id: profile.client_id.clone(),
      scopes: profile.scopes.clone(),
    }
  }

  /// The keyring entry of this server's refresh token: per origin, issuer and client, so a server
  /// that announces another identity provider never gets the previous provider's token.
  pub fn token_key(&self) -> String {
    format!("{}|{}|{}", self.server_url, self.issuer, self.client_id)
  }

  /// The directory name of this server's local data.
  pub fn storage_key(&self) -> Result<String, String> {
    ServerOrigin::parse(&self.server_url, HttpPolicy::Allowed)
      .map(|origin| origin.storage_key())
      .map_err(|error| error.to_string())
  }

  /// The dev stack of the README, overridden by environment variables (tests against it only).
  #[cfg(all(test, feature = "integration"))]
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
      scopes: vec!["openid".to_owned()],
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
  /// No server is chosen yet: the user has to enter one first.
  #[error("no server chosen")]
  NoServer,
  /// The server refused a local account's sign-in.
  #[error("the server refused the sign-in: {}", .0.kind())]
  LocalRefused(LocalRefusal),
}

/// Why a server's `GET /api/v1/server-info` could not be used.
#[derive(Debug, thiserror::Error)]
pub enum ServerProbeError {
  #[error("the server could not be reached: {0}")]
  Unreachable(String),
  /// The host name does not resolve. The UI may offer to look for a similar address
  /// ([`AuthService::suggest_server`]), only when the user asks for it.
  #[error("the host name is unknown: {0}")]
  UnknownHost(String),
  #[error("the secure connection failed: {0}")]
  Tls(String),
  #[error(transparent)]
  Check(#[from] ServerCheckError),
}

impl ServerProbeError {
  /// The i18n message of the UI.
  pub fn i18n_key(&self) -> &'static str {
    match self {
      Self::Unreachable(_) => "server-entry-unreachable",
      Self::UnknownHost(_) => "server-entry-unknown-host",
      Self::Tls(_) => "server-entry-tls",
      Self::Check(check) => check.i18n_key(),
    }
  }
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
      Self::NoServer => "no_server",
      Self::LocalRefused(refusal) => refusal.kind(),
    }
  }
}

/// Response of an authenticated API call, as handed to the webview.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ApiResponse {
  pub status: u16,
  pub body: String,
}

/// Response of an authenticated API call with a binary body; the bytes travel as base64.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ApiBytesResponse {
  pub status: u16,
  pub content_type: Option<String>,
  pub body_base64: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Discovery {
  issuer: String,
  authorization_endpoint: String,
  token_endpoint: String,
  #[serde(default)]
  end_session_endpoint: Option<String>,
}

/// The access token as it stands right now, read without waiting or refreshing
/// ([`AuthService::peek_server_bearer_token`]).
pub enum TokenPeek {
  /// The token of the active server and how long it may still be sent (expiry minus a margin).
  Ready {
    server_url: String,
    token: SecretString,
    valid_for: Duration,
  },
  /// No server, nobody signed in to it, or the token has (nearly) run out: no token is sent until
  /// the refresh loop replaced it.
  Unavailable,
  /// A sign-in or refresh holds the session: the caller may use the token it saw last, while that
  /// is still valid.
  Busy,
}

struct Tokens {
  access: SecretString,
  refresh: Option<SecretString>,
  expires_at: Instant,
}

#[derive(Default)]
struct Session {
  maybe_tokens: Option<Tokens>,
  /// Set instead of `maybe_tokens` while a local account is signed in.
  maybe_local: Option<LocalSession>,
  maybe_discovery: Option<Discovery>,
}

pub struct AuthService<S: TokenStore> {
  /// `None` until the user has chosen a server.
  server: RwLock<Option<DesktopConfig>>,
  http: reqwest::Client,
  store: S,
  session: Mutex<Session>,
  cancel: Notify,
}

impl<S: TokenStore> AuthService<S> {
  pub fn new(maybe_server: Option<DesktopConfig>, store: S) -> Result<Self, AuthError> {
    let http = reqwest::Client::builder()
      .timeout(HTTP_TIMEOUT)
      // A redirect would carry the bearer token to wherever a hostile answer points.
      .redirect(reqwest::redirect::Policy::none())
      .build()
      .map_err(|error| AuthError::Unsupported(error.to_string()))?;
    Ok(Self {
      server: RwLock::new(maybe_server),
      http,
      store,
      session: Mutex::new(Session::default()),
      cancel: Notify::new(),
    })
  }

  /// The active server, if one is chosen.
  pub fn active_server(&self) -> Option<DesktopConfig> {
    self.server.read().unwrap_or_else(PoisonError::into_inner).clone()
  }

  /// Makes `maybe_server` the active server. The session of the previous one ends here (its
  /// stored token stays with its own server).
  pub async fn set_server(&self, maybe_server: Option<DesktopConfig>) {
    let mut session = self.session.lock().await;
    *session = Session::default();
    *self.server.write().unwrap_or_else(PoisonError::into_inner) = maybe_server;
    self.cancel.notify_waiters();
  }

  /// Asks `origin` how to sign in to it (`GET /api/v1/server-info`) and checks the answer. Nothing
  /// changes until the user confirms; redirects are reported, never followed.
  pub async fn probe_server(&self, origin: &ServerOrigin) -> Result<CheckedServer, ServerProbeError> {
    let response = match self
      .http
      .get(origin.server_info_url())
      .header(reqwest::header::ACCEPT, "application/json")
      .send()
      .await
    {
      Ok(response) => response,
      Err(error) => return Err(probe_transport_error(origin, &error, resolves).await),
    };
    let status = response.status().as_u16();
    let maybe_content_type = response
      .headers()
      .get(reqwest::header::CONTENT_TYPE)
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned);
    let body = read_limited(response, MAX_SERVER_INFO_BYTES).await?;
    let answer = ServerAnswer {
      status,
      maybe_content_type: maybe_content_type.as_deref(),
      body: &body,
    };
    Ok(server_entry::check_server_info(
      origin,
      answer,
      env!("CARGO_PKG_VERSION"),
      Some(REDIRECT_URI_PATTERN),
      HTTP_POLICY,
    )?)
  }

  /// "Did you mean …?" for an address that does not resolve: the first sibling under another
  /// common top-level domain that does. Only on the user's request (it looks up several names).
  pub async fn suggest_server(&self, origin: &ServerOrigin) -> Option<ServerOrigin> {
    suggest_sibling(origin, resolves).await
  }

  /// Signs in with a stored local session or refresh token, if there is one. `Ok(false)` means
  /// "sign in again" (also when no server is chosen yet).
  pub async fn restore(&self) -> Result<bool, AuthError> {
    let Some(server) = self.active_server() else {
      return Ok(false);
    };
    if self.restore_local(&server).await? {
      return Ok(true);
    }
    let Some(refresh) = self.store.load(&server.token_key()).await? else {
      return Ok(false);
    };
    let mut session = self.session.lock().await;
    match self.refresh_with(&mut session, &server, refresh).await {
      Ok(()) => Ok(true),
      // The provider rejected the token (expired, revoked, user disabled): forget it.
      Err(AuthError::Denied(_) | AuthError::NotSignedIn) => {
        self.store.clear(&server.token_key()).await?;
        Ok(false)
      },
      Err(other) => Err(other),
    }
  }

  /// Runs the browser login. `open_browser` receives the authorization URL; `look` styles the
  /// page the browser shows when it comes back; `request` adds the login page, hint and language
  /// (issue #448). Returns what the next sign-in can prefill.
  pub async fn login(
    &self,
    look: &SignInLook,
    request: &LoginRequest<'_>,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
  ) -> Result<SignedIn, AuthError> {
    let server = self.server()?;
    let discovery = self.discovery(&server).await?;
    let (listener, redirect_uri) = loopback::bind().await?;
    let client = self.client(&discovery, &redirect_uri, &server)?;
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, state) = authorization_url(&client, &server.scopes, challenge, request);
    // The discovery was checked; check the final URL again right before the OS opens it.
    if !is_provider_endpoint(url.as_str(), HTTP_POLICY) {
      return Err(AuthError::InvalidResponse(
        "the sign-in address is not https".to_owned(),
      ));
    }
    open_browser(url.as_str())
      .map_err(|error| AuthError::Unsupported(format!("could not open the browser: {error}")))?;

    let wait = loopback::wait_for_redirect(listener, state.secret(), &redirect_uri, look);
    let redirect = tokio::select! {
      result = tokio::time::timeout(LOGIN_TIMEOUT, wait) => result.map_err(|_| AuthError::Cancelled)??,
      () = self.cancel.notified() => return Err(AuthError::Cancelled),
    };
    // The browser waits for its page until the tokens are in: it then says what really happened.
    let outcome = self
      .finish_login(&client, &server, redirect.params.code(), verifier)
      .await;
    redirect.answer(callback_outcome(&outcome), look).await;
    let signed_in = outcome?;
    // A local session of an earlier sign-in must not come back at the next start.
    if let Err(error) = self.store.clear_local_session(&server.server_url).await {
      tracing::warn!(%error, "could not forget the earlier local session");
    }
    Ok(signed_in)
  }

  /// Exchanges the redirect's code for tokens and keeps them.
  async fn finish_login(
    &self,
    client: &OidcClient,
    server: &DesktopConfig,
    code: Result<String, AuthError>,
    verifier: PkceCodeVerifier,
  ) -> Result<SignedIn, AuthError> {
    let http = self.oauth_http();
    let response = client
      .exchange_code(AuthorizationCode::new(code?))
      .set_pkce_verifier(verifier)
      .request_async(&http)
      .await
      .map_err(map_token_error)?;
    let mut session = self.session.lock().await;
    session.maybe_local = None;
    self.adopt(&mut session, server, &response).await?;
    Ok(SignedIn {
      maybe_login_hint: response.extra_fields().login_hint(),
    })
  }

  /// Signs in a local account with e-mail, password and, when given, the one-time code. The token
  /// is kept for the API and stored for the next start; there is no refresh, expiry signs out.
  pub async fn login_local(
    &self,
    login: &str,
    password: &SecretString,
    maybe_totp_code: Option<&str>,
  ) -> Result<(), AuthError> {
    let server = self.server()?;
    let body = local::login_body(login, password, maybe_totp_code)?;
    let url = format!("{}{LOCAL_LOGIN_PATH}", server.server_url.trim_end_matches('/'));
    let response = self
      .http
      .post(url)
      .header(reqwest::header::CONTENT_TYPE, "application/json")
      .header(reqwest::header::ACCEPT, "application/json")
      // reqwest takes the body by value; this copy is dropped with the request.
      .body(body.expose_secret().to_owned())
      .send()
      .await
      .map_err(|error| AuthError::Network(error.to_string()))?;
    let status = response.status().as_u16();
    let answer = response
      .bytes()
      .await
      .map_err(|error| AuthError::Network(error.to_string()))?;
    if status != 200 {
      return Err(match LocalRefusal::from_answer(status, &answer) {
        Some(refusal) => AuthError::LocalRefused(refusal),
        None => AuthError::InvalidResponse(format!("the local sign-in answered HTTP {status}")),
      });
    }
    let granted = LocalSession::from_response(&answer)?;
    self.adopt_local(&server, granted).await
  }

  /// Aborts a login that waits for the browser.
  pub fn cancel_login(&self) {
    self.cancel.notify_waiters();
  }

  /// Ends the session at the server or the provider (best effort) and forgets everything stored
  /// locally.
  pub async fn logout(&self) -> Result<(), AuthError> {
    let Some(server) = self.active_server() else {
      return Ok(());
    };
    let (in_memory, discovery, maybe_local) = {
      let mut session = self.session.lock().await;
      (
        session.maybe_tokens.take().and_then(|tokens| tokens.refresh),
        session.maybe_discovery.clone(),
        session.maybe_local.take(),
      )
    };
    self.logout_local(&server, maybe_local).await?;
    let refresh = match in_memory {
      Some(token) => Some(token),
      None => self.store.load(&server.token_key()).await?,
    };
    self.store.clear(&server.token_key()).await?;
    if let (Some(refresh), Some(endpoint)) = (refresh, discovery.and_then(|found| found.end_session_endpoint))
    {
      let form = [
        ("client_id", server.client_id.as_str()),
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
    self.api_request(ApiMethod::Get, path, None).await
  }

  /// `GET {server}{path}` without credentials, for the public endpoints the login screen needs
  /// (the brand of the server). Only `/api/...` paths; the answer is data for the page.
  pub async fn public_get(&self, path: &str) -> Result<ApiResponse, AuthError> {
    validate_api_path(path)?;
    let url = format!("{}{path}", self.server()?.server_url.trim_end_matches('/'));
    let response = self
      .http
      .get(url)
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

  /// A brand image of the active server (`/api/v1/brand/assets/{slot}`), without credentials:
  /// the webview's content security policy only loads images from itself or `data:` URLs, so
  /// the page shows it as one. Images only, at most [`MAX_BRAND_ASSET_BYTES`].
  pub async fn public_asset(&self, path: &str) -> Result<ApiBytesResponse, AuthError> {
    let server_url = self.server()?.server_url;
    self.brand_asset(&server_url, path).await
  }

  /// A brand image of the server at `server_url`, also one that is not confirmed yet (its
  /// confirmation shows the logo).
  pub async fn brand_asset(&self, server_url: &str, path: &str) -> Result<ApiBytesResponse, AuthError> {
    if !is_brand_asset_path(path) {
      return Err(AuthError::Unsupported(format!(
        "path {path:?} is not a brand image"
      )));
    }
    let url = format!("{}{path}", server_url.trim_end_matches('/'));
    let response = self
      .http
      .get(url)
      .send()
      .await
      .map_err(|error| AuthError::Network(error.to_string()))?;
    let status = response.status().as_u16();
    let content_type = response
      .headers()
      .get(reqwest::header::CONTENT_TYPE)
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned)
      .filter(|value| value.starts_with("image/"));
    let Some(content_type) = content_type else {
      return Ok(ApiBytesResponse {
        status,
        content_type: None,
        body_base64: String::new(),
      });
    };
    let mut body = Vec::new();
    let mut response = response;
    while let Some(chunk) = response
      .chunk()
      .await
      .map_err(|error| AuthError::Network(error.to_string()))?
    {
      if body.len() + chunk.len() > MAX_BRAND_ASSET_BYTES {
        return Err(AuthError::InvalidResponse(
          "the brand image is too large".to_owned(),
        ));
      }
      body.extend_from_slice(&chunk);
    }
    Ok(ApiBytesResponse {
      status,
      content_type: Some(content_type),
      body_base64: base64::engine::general_purpose::STANDARD.encode(&body),
    })
  }

  /// `{method} {server}{path}` with a fresh access token; a body is sent as JSON.
  pub async fn api_request(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<String>,
  ) -> Result<ApiResponse, AuthError> {
    validate_api_path(path)?;
    // One read of the server: the token and the URL always belong to the same one.
    let server = self.server()?;
    let token = self.access_token(&server).await?;
    let url = format!("{}{path}", server.server_url.trim_end_matches('/'));
    let mut request = self
      .http
      .request(reqwest_method(method), url)
      .bearer_auth(token.expose_secret())
      .header(reqwest::header::ACCEPT, "application/json");
    if let Some(body) = maybe_body {
      request = request
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);
    }
    let response = request
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

  /// `{method} {server}{path}` with a fresh access token and a binary body of the given type (brand
  /// images); the answer's body is returned as bytes too.
  pub async fn api_request_bytes(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<(String, Vec<u8>)>,
  ) -> Result<ApiBytesResponse, AuthError> {
    validate_api_path(path)?;
    let server = self.server()?;
    let token = self.access_token(&server).await?;
    let url = format!("{}{path}", server.server_url.trim_end_matches('/'));
    let mut request = self
      .http
      .request(reqwest_method(method), url)
      .bearer_auth(token.expose_secret());
    if let Some((content_type, bytes)) = maybe_body {
      request = request
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(bytes);
    }
    let response = request
      .send()
      .await
      .map_err(|error| AuthError::Network(error.to_string()))?;
    let status = response.status().as_u16();
    let content_type = response
      .headers()
      .get(reqwest::header::CONTENT_TYPE)
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned);
    let bytes = response
      .bytes()
      .await
      .map_err(|error| AuthError::Network(error.to_string()))?;
    Ok(ApiBytesResponse {
      status,
      content_type,
      body_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
  }

  /// The current access token as text, refreshed when needed (tests; the app uses
  /// [`Self::server_bearer_token`]).
  #[cfg(test)]
  pub async fn bearer_token(&self) -> Result<String, AuthError> {
    Ok(self.server_bearer_token().await?.1)
  }

  /// `(server origin, access token)`: the token with the server it is for.
  pub async fn server_bearer_token(&self) -> Result<(String, String), AuthError> {
    let server = self.server()?;
    let token = self.access_token(&server).await?;
    Ok((server.server_url, token.expose_secret().to_owned()))
  }

  /// The access token of the active server without waiting for the session or refreshing it, for
  /// the core's synchronous token source: a core started right after sign-in sees the new token
  /// at once instead of after the next refresh tick.
  pub fn peek_server_bearer_token(&self) -> TokenPeek {
    let Some(server) = self.active_server() else {
      return TokenPeek::Unavailable;
    };
    let Ok(session) = self.session.try_lock() else {
      return TokenPeek::Busy;
    };
    let maybe_token = match (&session.maybe_local, &session.maybe_tokens) {
      (Some(local), _) => local
        .remaining(local::now_ms())
        .map(|valid_for| (local.token(), valid_for)),
      (None, Some(tokens)) => {
        let valid_for = tokens.expires_at.saturating_duration_since(Instant::now());
        (valid_for > TOKEN_EXPIRY_MARGIN).then(|| {
          (
            SecretString::from(tokens.access.expose_secret().to_owned()),
            valid_for - TOKEN_EXPIRY_MARGIN,
          )
        })
      },
      (None, None) => None,
    };
    match maybe_token {
      Some((token, valid_for)) => TokenPeek::Ready {
        server_url: server.server_url,
        token,
        valid_for,
      },
      None => TokenPeek::Unavailable,
    }
  }

  /// Whether a session (identity provider or local account) exists.
  pub async fn is_signed_in(&self) -> bool {
    let session = self.session.lock().await;
    session.maybe_tokens.is_some() || session.maybe_local.is_some()
  }

  /// Deletes a refresh token stored under the server origin alone (the key before tokens were
  /// kept per origin, issuer and client), so it can never be used for another provider.
  pub async fn forget_origin_only_token(&self, server: &DesktopConfig) -> Result<(), AuthError> {
    self.store.clear(&server.server_url).await
  }

  /// Deletes the stored refresh token and local session of `server` (leaving it, or its sign-in
  /// settings changed).
  pub async fn forget_tokens(&self, server: &DesktopConfig) -> Result<(), AuthError> {
    self.store.clear(&server.token_key()).await?;
    self.store.clear_local_session(&server.server_url).await
  }

  /// The access token of `server`, refreshed when needed; refused when the active server is
  /// another one by now.
  async fn access_token(&self, server: &DesktopConfig) -> Result<SecretString, AuthError> {
    let mut session = self.session.lock().await;
    if self.active_server().as_ref() != Some(server) {
      return Err(AuthError::NotSignedIn);
    }
    if let Some(local) = &session.maybe_local {
      if local.is_live(local::now_ms()) {
        return Ok(local.token());
      }
      session.maybe_local = None;
      drop(session);
      if let Err(error) = self.store.clear_local_session(&server.server_url).await {
        tracing::warn!(%error, "could not forget the expired local session");
      }
      return Err(AuthError::NotSignedIn);
    }
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
      if let Err(error) = self.refresh_with(&mut session, server, refresh).await {
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

  async fn refresh_with(
    &self,
    session: &mut Session,
    server: &DesktopConfig,
    refresh: SecretString,
  ) -> Result<(), AuthError> {
    let discovery = self.discovery_in(session, server).await?;
    // The redirect URI is not used for refreshing, but the client needs a valid one.
    let client = self.client(&discovery, REDIRECT_URI_PATTERN, server)?;
    let http = self.oauth_http();
    let response = client
      .exchange_refresh_token(&RefreshToken::new(refresh.expose_secret().to_owned()))
      .request_async(&http)
      .await
      .map_err(map_token_error)?;
    self.adopt(session, server, &response).await
  }

  /// Takes over a token response of `server`: keeps the access token in memory and persists the
  /// refresh token. A response that arrives after the user switched servers is dropped.
  async fn adopt(
    &self,
    session: &mut Session,
    server: &DesktopConfig,
    response: &OidcTokenResponse,
  ) -> Result<(), AuthError> {
    if self.active_server().as_ref() != Some(server) {
      return Err(AuthError::Cancelled);
    }
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
      self.store.save(&server.token_key(), refresh).await?;
    }
    let lifetime = response.expires_in().unwrap_or(Duration::from_secs(60));
    session.maybe_tokens = Some(Tokens {
      access: SecretString::from(response.access_token().secret().clone()),
      refresh: refresh.map(SecretString::from),
      expires_at: Instant::now() + lifetime,
    });
    Ok(())
  }

  /// A stored local session that is still valid becomes the session; an expired or damaged one is
  /// forgotten.
  async fn restore_local(&self, server: &DesktopConfig) -> Result<bool, AuthError> {
    let Some(stored) = self.store.load_local_session(&server.server_url).await? else {
      return Ok(false);
    };
    match LocalSession::decode(&stored).filter(|session| session.is_live(local::now_ms())) {
      Some(session) => {
        let mut current = self.session.lock().await;
        current.maybe_tokens = None;
        current.maybe_local = Some(session);
        Ok(true)
      },
      None => {
        self.store.clear_local_session(&server.server_url).await?;
        Ok(false)
      },
    }
  }

  /// Takes over a granted local session of `server`, unless the user switched servers meanwhile.
  async fn adopt_local(&self, server: &DesktopConfig, granted: LocalSession) -> Result<(), AuthError> {
    if self.active_server().as_ref() != Some(server) {
      return Err(AuthError::Cancelled);
    }
    self
      .store
      .save_local_session(&server.server_url, &granted.encode()?)
      .await?;
    // A refresh token of an earlier identity provider sign-in must not come back at the next start.
    if let Err(error) = self.store.clear(&server.token_key()).await {
      tracing::warn!(%error, "could not forget the earlier refresh token");
    }
    let mut session = self.session.lock().await;
    session.maybe_tokens = None;
    session.maybe_local = Some(granted);
    Ok(())
  }

  /// Ends a local session at the server (best effort) and forgets the stored one.
  async fn logout_local(
    &self,
    server: &DesktopConfig,
    maybe_in_memory: Option<LocalSession>,
  ) -> Result<(), AuthError> {
    let maybe_session = match maybe_in_memory {
      Some(session) => Some(session),
      None => self
        .store
        .load_local_session(&server.server_url)
        .await?
        .and_then(|stored| LocalSession::decode(&stored)),
    };
    self.store.clear_local_session(&server.server_url).await?;
    let Some(session) = maybe_session.filter(|session| session.is_live(local::now_ms())) else {
      return Ok(());
    };
    let url = format!("{}{LOCAL_LOGOUT_PATH}", server.server_url.trim_end_matches('/'));
    if let Err(error) = self
      .http
      .post(url)
      .bearer_auth(session.access.expose_secret())
      .send()
      .await
    {
      // The local session is gone either way; the server's token expires on its own.
      tracing::warn!(%error, "could not end the local session on the server");
    }
    Ok(())
  }

  fn server(&self) -> Result<DesktopConfig, AuthError> {
    self.active_server().ok_or(AuthError::NoServer)
  }

  async fn discovery(&self, server: &DesktopConfig) -> Result<Discovery, AuthError> {
    let mut session = self.session.lock().await;
    self.discovery_in(&mut session, server).await
  }

  async fn discovery_in(
    &self,
    session: &mut Session,
    server: &DesktopConfig,
  ) -> Result<Discovery, AuthError> {
    if let Some(found) = &session.maybe_discovery {
      return Ok(found.clone());
    }
    let url = format!(
      "{}/.well-known/openid-configuration",
      server.issuer.trim_end_matches('/')
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
    // The endpoints reach the system browser and the token requests: https only, and the
    // document must be the configured issuer's (RFC 8414 §3.3).
    let mut endpoints = vec![
      found.authorization_endpoint.as_str(),
      found.token_endpoint.as_str(),
    ];
    endpoints.extend(found.end_session_endpoint.as_deref());
    check_discovery(&server.issuer, &found.issuer, &endpoints, HTTP_POLICY)
      .map_err(|error| AuthError::InvalidResponse(error.to_string()))?;
    session.maybe_discovery = Some(found.clone());
    Ok(found)
  }

  fn client(
    &self,
    discovery: &Discovery,
    redirect_uri: &str,
    server: &DesktopConfig,
  ) -> Result<OidcClient, AuthError> {
    let invalid = |error: url::ParseError| AuthError::InvalidResponse(error.to_string());
    Ok(oidc_client(
      ClientId::new(server.client_id.clone()),
      AuthUrl::new(discovery.authorization_endpoint.clone()).map_err(invalid)?,
      TokenUrl::new(discovery.token_endpoint.clone()).map_err(invalid)?,
      RedirectUrl::new(redirect_uri.to_owned()).map_err(invalid)?,
    ))
  }

  /// HTTP client of the `oauth2` crate on top of our `reqwest` client.
  fn oauth_http(&self) -> OAuthHttp {
    OAuthHttp(self.http.clone())
  }
}

/// What a finished browser sign-in leaves for the next one. No `Debug` with the account name.
#[derive(Default, PartialEq, Eq)]
pub struct SignedIn {
  /// The account the ID token names (`preferred_username` or `email`).
  pub maybe_login_hint: Option<String>,
}

impl std::fmt::Debug for SignedIn {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SignedIn")
      .field("has_login_hint", &self.maybe_login_hint.is_some())
      .finish()
  }
}

/// What the browser tab says about a finished sign-in.
fn callback_outcome<T>(outcome: &Result<T, AuthError>) -> CallbackOutcome {
  match outcome {
    Ok(_) => CallbackOutcome::SignedIn,
    Err(AuthError::Denied(_) | AuthError::Cancelled) => CallbackOutcome::Refused,
    Err(AuthError::StateMismatch) => CallbackOutcome::StateMismatch,
    Err(_) => CallbackOutcome::Failed,
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

/// Tells a host name that does not exist and a connection the TLS layer refused from a server
/// that is not there. The detail is the whole error chain, for the collapsed "details".
async fn probe_transport_error<F, Fut>(
  origin: &ServerOrigin,
  error: &reqwest::Error,
  resolves: F,
) -> ServerProbeError
where
  F: Fn(String) -> Fut,
  Fut: std::future::Future<Output = bool>,
{
  let mut source: Option<&dyn std::error::Error> = Some(error);
  let mut parts = Vec::new();
  while let Some(current) = source {
    parts.push(current.to_string());
    source = current.source();
  }
  let detail = parts.join(": ");
  let failure = classify_transport(error.is_connect(), &detail, origin, resolves).await;
  match failure {
    TransportFailure::UnknownHost => ServerProbeError::UnknownHost(detail),
    TransportFailure::Tls => ServerProbeError::Tls(detail),
    TransportFailure::Other => ServerProbeError::Unreachable(detail),
  }
}

#[derive(Debug, PartialEq, Eq)]
enum TransportFailure {
  UnknownHost,
  Tls,
  Other,
}

/// Sorts a failed request. A connection that failed for a host the resolver does not know is an
/// unknown host; this asks the resolver itself (the same name the user asked for) instead of
/// reading the resolver's message, which Windows translates. TLS failures are recognised by the
/// TLS stack's own (untranslated) wording.
async fn classify_transport<F, Fut>(
  is_connect: bool,
  chain: &str,
  origin: &ServerOrigin,
  resolves: F,
) -> TransportFailure
where
  F: Fn(String) -> Fut,
  Fut: std::future::Future<Output = bool>,
{
  let chain = chain.to_ascii_lowercase();
  let is_tls = ["certificate", "tls", "ssl", "handshake"]
    .iter()
    .any(|word| chain.contains(word));
  if is_tls {
    TransportFailure::Tls
  } else if is_connect && !resolves(lookup_authority(origin.host())).await {
    TransportFailure::UnknownHost
  } else {
    TransportFailure::Other
  }
}

/// `host:port` for the resolver; `https` origins without a port use 443.
fn lookup_authority(host: &str) -> String {
  let has_port = host
    .rsplit_once(':')
    .is_some_and(|(_, port)| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
    && !host.ends_with(']');
  if has_port {
    host.to_owned()
  } else {
    format!("{host}:443")
  }
}

/// How long the "did you mean …?" lookups may take together.
const SUGGESTION_TIMEOUT: Duration = Duration::from_secs(3);

/// The first sibling of `origin` (`.de` for `.com`, …) whose host name resolves, in the order of
/// [`ServerOrigin::sibling_suffixes`]. All are looked up at once; any not answered in
/// [`SUGGESTION_TIMEOUT`] count as unknown.
async fn suggest_sibling<F, Fut>(origin: &ServerOrigin, resolves: F) -> Option<ServerOrigin>
where
  F: Fn(String) -> Fut,
  Fut: std::future::Future<Output = bool> + Send + 'static,
{
  let candidates = origin.sibling_suffixes();
  let mut checks = tokio::task::JoinSet::new();
  for (index, candidate) in candidates.iter().enumerate() {
    let check = resolves(lookup_authority(candidate.host()));
    checks.spawn(async move { (index, check.await) });
  }
  let mut found = vec![false; candidates.len()];
  // A slow resolver leaves the rest unknown; dropping the set stops the lookups.
  let _ = tokio::time::timeout(SUGGESTION_TIMEOUT, async {
    while let Some(Ok((index, is_known))) = checks.join_next().await {
      found[index] = is_known;
    }
  })
  .await;
  candidates
    .into_iter()
    .zip(found)
    .find_map(|(candidate, is_known)| is_known.then_some(candidate))
}

/// Whether `authority` (`host:port`) resolves with the system resolver within
/// [`SUGGESTION_TIMEOUT`]; a resolver that does not answer counts as "no".
async fn resolves(authority: String) -> bool {
  tokio::time::timeout(SUGGESTION_TIMEOUT, tokio::net::lookup_host(authority))
    .await
    .is_ok_and(|lookup| lookup.is_ok_and(|mut addresses| addresses.next().is_some()))
}

/// The body, up to `limit` bytes; more is refused (a hostile or broken answer).
async fn read_limited(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, ServerProbeError> {
  let mut body = Vec::new();
  while let Some(chunk) = response
    .chunk()
    .await
    .map_err(|error| ServerProbeError::Unreachable(error.to_string()))?
  {
    if body.len() + chunk.len() > limit {
      return Err(ServerProbeError::Check(ServerCheckError::NotTeamCraft));
    }
    body.extend_from_slice(&chunk);
  }
  Ok(body)
}

/// `/api/v1/brand/assets/<slot>` with a plain slot name, nothing else.
fn is_brand_asset_path(path: &str) -> bool {
  path.strip_prefix(BRAND_ASSET_PREFIX).is_some_and(|slot| {
    !slot.is_empty() && slot.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_')
  })
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

fn reqwest_method(method: ApiMethod) -> reqwest::Method {
  match method {
    ApiMethod::Get => reqwest::Method::GET,
    ApiMethod::Post => reqwest::Method::POST,
    ApiMethod::Put => reqwest::Method::PUT,
    ApiMethod::Patch => reqwest::Method::PATCH,
    ApiMethod::Delete => reqwest::Method::DELETE,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod server_typos {
    use super::*;

    #[tokio::test]
    async fn an_unknown_host_is_what_the_resolver_says_not_its_wording() {
      let origin = ServerOrigin::parse("chat.example.com", HttpPolicy::LoopbackOnly).unwrap();
      // A localized Windows message, unknown to any word list: the resolver decides.
      let localized = "Fehler beim Verbinden: Der angegebene Host ist unbekannt. (os error 11001)";
      let unknown = classify_transport(true, localized, &origin, |authority| async move {
        assert_eq!(authority, "chat.example.com:443");
        false
      })
      .await;
      assert_eq!(unknown, TransportFailure::UnknownHost);
      let refused = classify_transport(true, "connection refused", &origin, |_| async { true }).await;
      assert_eq!(refused, TransportFailure::Other);
      // Not a connect error (a timeout while reading): never looked up.
      let timeout = classify_transport(
        false,
        "operation timed out",
        &origin,
        |_: String| -> std::future::Ready<bool> { unreachable!("no lookup for a read error") },
      )
      .await;
      assert_eq!(timeout, TransportFailure::Other);
      let tls = classify_transport(
        true,
        "invalid peer certificate: UnknownIssuer",
        &origin,
        |_: String| -> std::future::Ready<bool> { unreachable!("no lookup for a TLS error") },
      )
      .await;
      assert_eq!(tls, TransportFailure::Tls);
    }

    #[test]
    fn lookups_name_the_port() {
      assert_eq!(lookup_authority("chat.example.com"), "chat.example.com:443");
      assert_eq!(lookup_authority("chat.example.com:8443"), "chat.example.com:8443");
      assert_eq!(lookup_authority("[2001:db8::1]"), "[2001:db8::1]:443");
      assert_eq!(lookup_authority("[2001:db8::1]:8080"), "[2001:db8::1]:8080");
    }

    #[tokio::test]
    async fn the_first_resolving_sibling_is_suggested() {
      let origin = ServerOrigin::parse("chat.example.com", HttpPolicy::LoopbackOnly).unwrap();
      let suggestion = suggest_sibling(&origin, |authority| async move {
        // `.de` does not exist here, `.net` and `.org` do.
        authority == "chat.example.net:443" || authority == "chat.example.org:443"
      })
      .await;
      assert_eq!(
        suggestion.as_ref().map(ServerOrigin::as_str),
        Some("https://chat.example.net")
      );
    }

    #[tokio::test]
    async fn nothing_resolving_means_no_suggestion() {
      let origin = ServerOrigin::parse("chat.example.com", HttpPolicy::LoopbackOnly).unwrap();
      assert_eq!(suggest_sibling(&origin, |_| async { false }).await, None);
      let local = ServerOrigin::parse("localhost:8080", HttpPolicy::Allowed).unwrap();
      assert_eq!(suggest_sibling(&local, |_| async { true }).await, None);
    }

    #[test]
    fn an_unknown_host_has_its_own_message() {
      assert_eq!(
        ServerProbeError::UnknownHost("dns".into()).i18n_key(),
        "server-entry-unknown-host"
      );
    }
  }

  /// Issue #448: an interactive sign-in asks for the login page, prefills the last account and
  /// matches the app's language; with the instance setting off and nothing known, none of that.
  #[test]
  fn the_authorization_url_asks_for_the_login_page_with_hint_and_language() {
    let client = oidc_client(
      ClientId::new("teamcraft".into()),
      AuthUrl::new("https://auth.example.com/authorize".into()).unwrap(),
      TokenUrl::new("https://auth.example.com/token".into()).unwrap(),
      RedirectUrl::new("http://127.0.0.1:4711/callback".into()).unwrap(),
    );
    let scopes = vec!["openid".to_owned()];
    let request = LoginRequest {
      is_login_page_shown: true,
      maybe_login_hint: Some("alice@example.com"),
      maybe_locale: Some("de"),
    };
    let (challenge, _) = PkceCodeChallenge::new_random_sha256();
    let (url, _) = authorization_url(&client, &scopes, challenge, &request);
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["prompt"], "login");
    assert_eq!(query["login_hint"], "alice@example.com");
    assert_eq!(query["ui_locales"], "de");
    assert_eq!(query["code_challenge_method"], "S256");

    let (challenge, _) = PkceCodeChallenge::new_random_sha256();
    let (url, _) = authorization_url(&client, &scopes, challenge, &LoginRequest::default());
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert!(
      !query.contains_key("prompt") && !query.contains_key("login_hint") && !query.contains_key("ui_locales")
    );
  }

  #[test]
  fn sign_in_outcomes_map_to_the_browser_page() {
    assert_eq!(
      callback_outcome(&Ok(SignedIn::default())),
      CallbackOutcome::SignedIn
    );
    assert_eq!(
      callback_outcome::<()>(&Err(AuthError::Denied("access_denied".into()))),
      CallbackOutcome::Refused
    );
    assert_eq!(
      callback_outcome::<()>(&Err(AuthError::StateMismatch)),
      CallbackOutcome::StateMismatch
    );
    assert_eq!(
      callback_outcome::<()>(&Err(AuthError::Network("down".into()))),
      CallbackOutcome::Failed
    );
  }

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
  fn refresh_tokens_are_kept_per_server_issuer_and_client() {
    let config = DesktopConfig {
      server_url: "https://team.example.com".into(),
      issuer: "https://auth.example.com/realms/a".into(),
      client_id: "teamcraft".into(),
      scopes: vec!["openid".into()],
    };
    let other_issuer = DesktopConfig {
      issuer: "https://evil.example".into(),
      ..config.clone()
    };
    let other_client = DesktopConfig {
      client_id: "other".into(),
      ..config.clone()
    };
    assert_ne!(config.token_key(), other_issuer.token_key());
    assert_ne!(config.token_key(), other_client.token_key());
  }

  #[test]
  fn only_brand_images_are_fetched_without_credentials() {
    assert!(is_brand_asset_path("/api/v1/brand/assets/logo_dark"));
    for bad in [
      "/api/v1/brand/assets/",
      "/api/v1/brand/assets/../me",
      "/api/v1/brand/assets/logo?x=1",
      "/api/v1/me",
      "https://evil.example/api/v1/brand/assets/logo",
    ] {
      assert!(!is_brand_asset_path(bad), "{bad}");
    }
  }

  #[test]
  fn error_kinds_are_stable() {
    assert_eq!(AuthError::Cancelled.kind(), "cancelled");
    assert_eq!(AuthError::Network("x".into()).kind(), "network");
    assert_eq!(AuthError::StateMismatch.kind(), "state_mismatch");
    assert_eq!(AuthError::LocalRefused(LocalRefusal::Refused).kind(), "refused");
    assert_eq!(AuthError::LocalRefused(LocalRefusal::Disabled).kind(), "disabled");
    assert_eq!(
      AuthError::LocalRefused(LocalRefusal::RateLimited).kind(),
      "rate_limited"
    );
  }

  mod local_sessions {
    use super::*;
    use crate::auth::store::MemoryStore;

    const SERVER: &str = "https://team.example.com";

    #[tokio::test]
    async fn a_stored_session_is_restored_and_signs_requests() {
      let store = MemoryStore::default();
      store
        .save_local_session(SERVER, &session("local-token", 3_600_000).encode().unwrap())
        .await
        .unwrap();
      let service = AuthService::new(Some(config()), store).unwrap();
      assert!(service.restore().await.unwrap());
      assert_eq!(service.bearer_token().await.unwrap(), "local-token");
    }

    #[tokio::test]
    async fn an_expired_session_is_forgotten_at_restore() {
      let store = MemoryStore::default();
      store
        .save_local_session(SERVER, &session("old", -1_000).encode().unwrap())
        .await
        .unwrap();
      let service = AuthService::new(Some(config()), store).unwrap();
      assert!(!service.restore().await.unwrap());
      assert!(service.store.load_local_session(SERVER).await.unwrap().is_none());
      assert!(matches!(
        service.bearer_token().await,
        Err(AuthError::NotSignedIn)
      ));
    }

    #[tokio::test]
    async fn expiry_while_running_signs_out() {
      let service = AuthService::new(Some(config()), MemoryStore::default()).unwrap();
      service
        .adopt_local(&config(), session("short", 1_000))
        .await
        .unwrap();
      assert!(service.store.load_local_session(SERVER).await.unwrap().is_some());
      assert!(matches!(
        service.bearer_token().await,
        Err(AuthError::NotSignedIn)
      ));
      assert!(service.store.load_local_session(SERVER).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_local_sign_in_replaces_a_stored_refresh_token() {
      let store = MemoryStore::default();
      store.save(&config().token_key(), "refresh").await.unwrap();
      let service = AuthService::new(Some(config()), store).unwrap();
      service
        .adopt_local(&config(), session("fresh", 3_600_000))
        .await
        .unwrap();
      assert!(service.store.load(&config().token_key()).await.unwrap().is_none());
      assert_eq!(service.bearer_token().await.unwrap(), "fresh");
    }

    #[tokio::test]
    async fn a_session_for_another_server_is_dropped() {
      let service = AuthService::new(Some(config()), MemoryStore::default()).unwrap();
      let mut other = config();
      other.server_url = "https://other.example.com".into();
      assert!(matches!(
        service.adopt_local(&other, session("x", 3_600_000)).await,
        Err(AuthError::Cancelled)
      ));
    }

    #[tokio::test]
    async fn logout_forgets_an_expired_session_without_calling_the_server() {
      let store = MemoryStore::default();
      store
        .save_local_session(SERVER, &session("old", -1_000).encode().unwrap())
        .await
        .unwrap();
      let service = AuthService::new(Some(config()), store).unwrap();
      service.logout().await.unwrap();
      assert!(service.store.load_local_session(SERVER).await.unwrap().is_none());
    }

    fn session(token: &str, lifetime_ms: i64) -> LocalSession {
      LocalSession {
        access: SecretString::from(token.to_owned()),
        expires_at_ms: local::now_ms() + lifetime_ms,
      }
    }

    fn config() -> DesktopConfig {
      DesktopConfig {
        server_url: SERVER.into(),
        issuer: "https://auth.example.com/realms/company".into(),
        client_id: "teamcraft".into(),
        scopes: vec!["openid".into()],
      }
    }
  }
}
