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
mod local;
mod loopback;
pub mod servers;
mod store;
#[cfg(test)]
mod test_dir;

#[cfg(all(test, feature = "integration"))]
pub(crate) mod integration_tests;

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError, RwLock};
use std::time::Duration;

use base64::Engine;
use oauth2::basic::BasicErrorResponseType;
use oauth2::{
  AuthUrl, AuthorizationCode, ClientId, CsrfToken, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl,
  RefreshToken, RequestTokenError, StandardErrorResponse, TokenResponse, TokenUrl,
};
use reqwest::header::{AUTHORIZATION, DATE, HeaderMap};
use secrecy::zeroize::Zeroizing;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Notify};

use teamcraft_client_api::oidc_login::{
  AuthorizationErrorKind, LoginRequest, OidcClient, OidcTokenResponse, answer_subject, authorization_url,
  oidc_client,
};
use teamcraft_client_api::remote::ApiMethod;
use teamcraft_client_api::server_entry::{
  self, CheckedServer, HttpPolicy, ServerAnswer, ServerCheckError, ServerOrigin, ServerProfile,
  check_discovery, is_provider_endpoint,
};
use teamcraft_client_api::sign_in_look::SignInLook;
use teamcraft_core::api::dpop::{
  Authorization as TokenAuthorization, DPOP_ALGORITHM_FOR_DISCOVERY, DPOP_HEADER, DPOP_NONCE_HEADER,
  DpopAnswer, DpopProofError, DpopProver, DpopSendError, Learned, SoftwareDpopSigner, TokenEndpointAttempt,
  signed_headers, with_token_endpoint_proof,
};
use teamcraft_core::store::SessionNote;
use teamcraft_crypto::dpop::{DpopSigningKey, JwkThumbprint, token_bound_key};
use teamcraft_proto::{ErrorBody, GetMeResponse, UserId};

use callback_page::CallbackOutcome;
use local::{LOCAL_LOGIN_PATH, LOCAL_LOGOUT_PATH, LocalRefusal, LocalSession};
#[cfg(test)]
pub(crate) use store::MemoryStore;
pub use store::{TokenStore, VaultStore};

/// The one request that goes out before the server confirmed a cached account, and the answer that
/// confirms it.
const ME_PATH: &str = "/api/v1/me";
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
  /// The identity provider answered the sign-in redirect with an `error` (RFC 6749 §4.1.2.1),
  /// cleaned by `shown_error_code` (issue #1063).
  #[error("the identity provider refused the sign-in: {0}")]
  AuthorizationRefused(String),
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
  /// The sign-in is bound to a key this computer doesn't have any more (e2ee.md §3.1.2).
  #[error("the key this sign-in is bound to is gone")]
  KeyLost,
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
      Self::AuthorizationRefused(_) => "authorization_refused",
      Self::Network(_) => "network",
      Self::InvalidResponse(_) => "invalid_response",
      Self::StateMismatch => "state_mismatch",
      Self::NotSignedIn => "not_signed_in",
      Self::Unsupported(_) => "unsupported",
      Self::NoServer => "no_server",
      Self::LocalRefused(refusal) => refusal.kind(),
      Self::KeyLost => "key_lost",
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
  /// RFC 9449 §5.1: the provider takes proofs signed so.
  #[serde(default)]
  dpop_signing_alg_values_supported: Vec<String>,
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
  /// No server, or nobody signed in to it.
  Unavailable,
  /// The access token has (nearly) run out and the refresh token waits to replace it, e.g. after
  /// a long suspend until the network is back: the session is fine, the app is just offline for
  /// now (issue #883).
  Refreshing,
  /// The identity provider refused the refresh token (`invalid_grant`): the user has to sign in
  /// again.
  Expired,
  /// The session was bound to a key this computer doesn't have any more: the user has to sign in
  /// again, and is told why.
  KeyLost,
  /// A sign-in or refresh holds the session: the caller may use the token it saw last, while that
  /// is still valid.
  Busy,
}

/// Where a token answer comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenOrigin {
  /// A sign-in: whoever it is, nothing stored about the session before it counts any more.
  NewSignIn,
  /// A refresh of the session there is.
  SameSession,
}

/// The account for the next start and the session it was confirmed for.
#[derive(Deserialize, Serialize)]
struct CachedAccountRecord {
  subject: String,
  account: GetMeResponse,
}

struct Tokens {
  access: SecretString,
  refresh: Option<SecretString>,
  /// Wall clock (Unix ms), not `Instant`: a monotonic clock stands still while the computer
  /// sleeps, so a token that expired during a suspend would still look valid (issue #883).
  expires_at_ms: i64,
  /// When the loop refreshes it (Unix ms): [`REFRESH_MARGIN`] before expiry, or halfway through a
  /// shorter lifetime, so a provider's one-minute tokens aren't refreshed every check.
  refresh_at_ms: i64,
  /// Obtained because the server refused the previous token: if the server refuses this one too,
  /// its answer stands and nothing is refreshed again (issue #883).
  is_from_refusal: bool,
}

impl Tokens {
  /// How long the access token is still valid at `now_ms`; zero once it has run out.
  fn remaining(&self, now_ms: i64) -> Duration {
    Duration::from_millis(u64::try_from(self.expires_at_ms - now_ms).unwrap_or(0))
  }
}

/// A stable fingerprint of an access token, so a caller can say which token was refused without
/// keeping or comparing the secret itself.
pub fn token_fingerprint(token: &str) -> u64 {
  let mut hasher = DefaultHasher::new();
  token.hash(&mut hasher);
  hasher.finish()
}

/// The wall clock in Unix milliseconds; tests inject their own.
type WallClock = Arc<dyn Fn() -> i64 + Send + Sync>;

#[derive(Default)]
struct Session {
  maybe_tokens: Option<Tokens>,
  /// Set instead of `maybe_tokens` while a local account is signed in.
  maybe_local: Option<LocalSession>,
  maybe_discovery: Option<Discovery>,
  /// The identity provider refused the refresh token since the last sign-in.
  is_expired: bool,
  /// The session's refresh token is bound to a key that is gone.
  is_key_lost: bool,
  /// The DPoP key with what is known about the identity provider.
  maybe_provider_prover: Option<Arc<DpopProver>>,
}

/// One answer of the server to a signed request, read whole: the one more try may need its body.
struct SignedAnswer {
  status: u16,
  maybe_content_type: Option<String>,
  body: Vec<u8>,
}

/// What a token request does about a proof.
enum TokenProof {
  /// The provider doesn't take proofs, or the server wouldn't check them: an unbound session.
  Unbound,
  With(Arc<DpopProver>),
}

pub struct AuthService<S: TokenStore> {
  /// `None` until the user has chosen a server.
  server: RwLock<Option<DesktopConfig>>,
  /// Replaced by [`AuthService::drop_connections`] after a wake-up or network change.
  http: RwLock<reqwest::Client>,
  store: S,
  session: Mutex<Session>,
  /// The app started from the cached account and the server hasn't confirmed it: nothing goes out
  /// and the core has no token, which reads as a wait, not as nobody being signed in (issue #778).
  is_unconfirmed: AtomicBool,
  /// The user the server named last for this session (`GET /me`). A confirmation that arrives later
  /// than a new sign-in doesn't name this user, and isn't kept.
  maybe_confirmed_user: StdMutex<Option<UserId>>,
  /// The active server's DPoP key with what is known about the server, once it was read or made. The
  /// core reads it without waiting for the session, like its token.
  maybe_prover: StdMutex<Option<Arc<DpopProver>>>,
  cancel: Notify,
  now_ms: WallClock,
}

impl<S: TokenStore> AuthService<S> {
  pub fn new(maybe_server: Option<DesktopConfig>, store: S) -> Result<Self, AuthError> {
    Ok(Self {
      server: RwLock::new(maybe_server),
      http: RwLock::new(http_client()?),
      store,
      session: Mutex::new(Session::default()),
      is_unconfirmed: AtomicBool::new(false),
      maybe_confirmed_user: StdMutex::new(None),
      maybe_prover: StdMutex::new(None),
      cancel: Notify::new(),
      now_ms: Arc::new(local::now_ms),
    })
  }

  /// The service with an injected wall clock (tests).
  #[cfg(test)]
  fn with_clock(maybe_server: Option<DesktopConfig>, store: S, now_ms: WallClock) -> Self {
    let mut service = Self::new(maybe_server, store).expect("a plain reqwest client builds");
    service.now_ms = now_ms;
    service
  }

  /// Forgets the pooled connections, which a suspend or a network change usually leaves dead: a
  /// request on one would hang until the connection times out (issue #883).
  pub fn drop_connections(&self) {
    match http_client() {
      Ok(client) => *self.http.write().unwrap_or_else(PoisonError::into_inner) = client,
      Err(error) => tracing::warn!(%error, "could not rebuild the HTTP client"),
    }
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
    self.is_unconfirmed.store(false, Ordering::Release);
    *self.confirmed_user() = None;
    *self.prover_slot() = None;
    *self.server.write().unwrap_or_else(PoisonError::into_inner) = maybe_server;
    self.cancel.notify_waiters();
  }

  /// Asks `origin` how to sign in to it (`GET /api/v1/server-info`) and checks the answer. Nothing
  /// changes until the user confirms; redirects are reported, never followed.
  pub async fn probe_server(&self, origin: &ServerOrigin) -> Result<CheckedServer, ServerProbeError> {
    let response = match self
      .http()
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
  /// "sign in" (also when no server is chosen yet); [`AuthError::Denied`] means the provider
  /// refused the stored token, so the sign-in screen says the session expired.
  pub async fn restore(&self) -> Result<bool, AuthError> {
    let outcome = self.restore_session().await;
    // Without a verdict (no answer, an answer that makes no sense) the session stays as it is and
    // the UI asks again. A session that is over takes the account along. One that is fine still
    // waits for the server to confirm the account.
    if matches!(
      outcome,
      Ok(false) | Err(AuthError::Denied(_) | AuthError::NotSignedIn | AuthError::KeyLost)
    ) {
      self.forget_cached_account().await;
    }
    outcome
  }

  /// The account the UI can start from at once, without the network, while [`Self::restore`] runs
  /// beside it (issue #778): the one confirmed last on the active server, if it was confirmed for the
  /// session stored now. It shows local data and proves nothing, and nothing goes out until the server
  /// confirmed it ([`Self::remember_account`]). Reads the same sealed file as the tokens: no keychain
  /// access beyond the one of the start.
  pub async fn cached_account(&self) -> Option<GetMeResponse> {
    let server = self.active_server()?;
    let subject = self.stored_session_subject(&server).await?;
    let found = self
      .store
      .load_note(SessionNote::CachedAccount, &server.server_url)
      .await;
    let json = found
      .inspect_err(|error| tracing::debug!(%error, "could not read the cached account"))
      .ok()??;
    let record: CachedAccountRecord = serde_json::from_str(&json).ok()?;
    if record.subject != subject {
      return None;
    }
    self.is_unconfirmed.store(true, Ordering::Release);
    Some(record.account)
  }

  /// The server confirmed `account`: the core gets its token, and the next start can begin with it,
  /// bound to the session it was confirmed for. Not kept when it isn't who the server named last for
  /// this session (a sign-out or a new sign-in while the answer was on its way), or when the session
  /// names nobody.
  pub async fn remember_account(&self, account: &GetMeResponse) {
    self.is_unconfirmed.store(false, Ordering::Release);
    let Some(server) = self.active_server() else {
      return;
    };
    if *self.confirmed_user() != Some(account.id) {
      tracing::info!("not keeping an account the server didn't name for this session");
      return;
    }
    let Some(subject) = self.stored_session_subject(&server).await else {
      return;
    };
    let record = CachedAccountRecord {
      subject,
      account: account.clone(),
    };
    let saved = match serde_json::to_string(&record) {
      Ok(json) => {
        self
          .store
          .save_note(SessionNote::CachedAccount, &server.server_url, &json)
          .await
      },
      Err(error) => Err(AuthError::InvalidResponse(error.to_string())),
    };
    if let Err(error) = saved {
      // The app works; the next start only waits for the server again.
      tracing::warn!(%error, "could not keep the account for the next start");
    }
  }

  /// Forgets the cached account of the active server: the next start waits for the server again.
  pub async fn forget_cached_account(&self) {
    self.is_unconfirmed.store(false, Ordering::Release);
    let Some(server) = self.active_server() else {
      return;
    };
    if let Err(error) = self
      .store
      .clear_note(SessionNote::CachedAccount, &server.server_url)
      .await
    {
      tracing::warn!(%error, "could not forget the cached account");
    }
  }

  /// Whether the app started from the cached account and the server hasn't confirmed it yet.
  pub fn is_unconfirmed(&self) -> bool {
    self.is_unconfirmed.load(Ordering::Acquire)
  }

  /// Who the stored session of `server` belongs to: its live local session, or the account at the
  /// identity provider its refresh token was issued to. `None` without a session, or for one that
  /// names nobody (opaque tokens without an ID token): that one gets no cached start.
  async fn stored_session_subject(&self, server: &DesktopConfig) -> Option<String> {
    let local = self.store.load_local_session(&server.server_url).await;
    if let Ok(Some(stored)) = local
      && let Some(session) = LocalSession::decode(&stored).filter(|session| session.is_live(local::now_ms()))
    {
      return Some(format!(
        "local:{:016x}",
        token_fingerprint(session.token().expose_secret())
      ));
    }
    let key = server.token_key();
    self.store.load(&key).await.ok()??;
    self.store.load_note(SessionNote::TokenSubject, &key).await.ok()?
  }

  fn confirmed_user(&self) -> std::sync::MutexGuard<'_, Option<UserId>> {
    self
      .maybe_confirmed_user
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
  }

  async fn restore_session(&self) -> Result<bool, AuthError> {
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
      Err(AuthError::Denied(reason)) => {
        self.store.clear(&server.token_key()).await?;
        Err(AuthError::Denied(reason))
      },
      // The refresh token only works with a key that is gone: it goes too, and the user is told.
      Err(AuthError::KeyLost) => {
        session.is_key_lost = true;
        self.store.clear(&server.token_key()).await?;
        self
          .store
          .clear_note(SessionNote::TokenBinding, &server.token_key())
          .await?;
        Err(AuthError::KeyLost)
      },
      Err(AuthError::NotSignedIn) => {
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
    // Whoever signs in now, the tokens saved below are not the cached account's any more.
    *self.confirmed_user() = None;
    self.forget_cached_account().await;
    let discovery = self.discovery(&server).await?;
    let (mut listener, mut attempt) = self.authorization(&discovery, &server, request).await?;
    open_browser(attempt.url.as_str())
      .map_err(|error| AuthError::Unsupported(format!("could not open the browser: {error}")))?;

    let mut retries = 0;
    let redirect = loop {
      let wait = loopback::wait_for_redirect(listener, attempt.state.secret(), &attempt.redirect_uri, look);
      let redirect = tokio::select! {
        result = tokio::time::timeout(LOGIN_TIMEOUT, wait) => result.map_err(|_| AuthError::Cancelled)??,
        () = self.cancel.notified() => return Err(AuthError::Cancelled),
      };
      let Some(code) = redirect.params.maybe_refusal() else {
        break redirect;
      };
      let kind = AuthorizationErrorKind::from_code(&code);
      if !kind.is_retried(retries) {
        tracing::warn!(error_code = %code, ?kind, "the identity provider refused the sign-in");
        break redirect;
      }
      // `already_logged_in` (issue #1063): the same tab goes to the provider again, with a fresh
      // state and verifier and the same request, so `prompt=login` shows the login page again.
      tracing::warn!(error_code = %code, ?kind, "the identity provider refused the sign-in, trying again");
      retries += 1;
      (listener, attempt) = match self.authorization(&discovery, &server, request).await {
        Ok(next) => next,
        Err(error) => {
          redirect.answer(CallbackOutcome::Failed, look).await;
          return Err(error);
        },
      };
      redirect.send_to(attempt.url.as_str()).await;
    };
    // The browser waits for its page until the tokens are in: it then says what really happened.
    let outcome = self
      .finish_login(&attempt.client, &server, redirect.params.code(), attempt.verifier)
      .await;
    redirect.answer(callback_outcome(&outcome), look).await;
    let signed_in = outcome?;
    // A local session of an earlier sign-in must not come back at the next start.
    if let Err(error) = self.store.clear_local_session(&server.server_url).await {
      tracing::warn!(%error, "could not forget the earlier local session");
    }
    Ok(signed_in)
  }

  /// A new authorization request: its loopback listener, a fresh `state` and PKCE verifier, and
  /// the provider's URL to open.
  async fn authorization(
    &self,
    discovery: &Discovery,
    server: &DesktopConfig,
    request: &LoginRequest<'_>,
  ) -> Result<(TcpListener, Authorization), AuthError> {
    let (listener, redirect_uri) = loopback::bind().await?;
    let client = self.client(discovery, &redirect_uri, server)?;
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, state) = authorization_url(&client, &server.scopes, challenge, request);
    // The discovery was checked; check the final URL again right before the browser goes there.
    if !is_provider_endpoint(url.as_str(), HTTP_POLICY) {
      return Err(AuthError::InvalidResponse(
        "the sign-in address is not https".to_owned(),
      ));
    }
    let authorization = Authorization {
      redirect_uri,
      client,
      verifier,
      state,
      url,
    };
    Ok((listener, authorization))
  }

  /// Exchanges the redirect's code for tokens and keeps them.
  async fn finish_login(
    &self,
    client: &OidcClient,
    server: &DesktopConfig,
    code: Result<String, AuthError>,
    verifier: PkceCodeVerifier,
  ) -> Result<SignedIn, AuthError> {
    let code = code?;
    let token_proof = {
      let mut session = self.session.lock().await;
      let discovery = self.discovery_in(&mut session, server).await?;
      self.new_session_proof(&mut session, server, &discovery).await
    };
    let http = self.oauth_http(&token_proof);
    let response = client
      .exchange_code(AuthorizationCode::new(code))
      .set_pkce_verifier(verifier)
      .request_async(&http)
      .await
      .map_err(map_token_error)?;
    let mut session = self.session.lock().await;
    session.maybe_local = None;
    self
      .adopt(&mut session, server, &response, TokenOrigin::NewSignIn)
      .await?;
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
    *self.confirmed_user() = None;
    self.forget_cached_account().await;
    let body = local::login_body(login, password, maybe_totp_code)?;
    let url = format!("{}{LOCAL_LOGIN_PATH}", server.server_url.trim_end_matches('/'));
    let response = self
      .http()
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
    let maybe_end_session_prover = self.end_session_prover(&server).await;
    let (in_memory, discovery, maybe_local) = {
      let mut session = self.session.lock().await;
      (
        session.maybe_tokens.take().and_then(|tokens| tokens.refresh),
        session.maybe_discovery.clone(),
        session.maybe_local.take(),
      )
    };
    let local = self.logout_local(&server, maybe_local).await;
    let refresh = match in_memory {
      Some(token) => Some(token),
      None => self
        .store
        .load(&server.token_key())
        .await
        .unwrap_or_else(|error| {
          tracing::warn!(%error, "could not read the refresh token to end the provider session");
          None
        }),
    };
    // Whatever happens to the local copy, the provider still hears of the sign-out: the token was
    // taken above, and a retry couldn't end the session any more.
    let cleared = self.store.clear(&server.token_key()).await;
    if let Err(error) = self
      .store
      .clear_note(SessionNote::TokenBinding, &server.token_key())
      .await
    {
      tracing::warn!(%error, "could not forget the sign-in's binding");
    }
    *self.confirmed_user() = None;
    self.forget_cached_account().await;
    if let (Some(refresh), Some(endpoint)) = (refresh, discovery.and_then(|found| found.end_session_endpoint))
    {
      let form = [
        ("client_id", server.client_id.as_str()),
        ("refresh_token", refresh.expose_secret()),
      ];
      let mut request = self.http().post(&endpoint);
      if let Some(prover) = maybe_end_session_prover {
        // The provider asks for the proof of a bound refresh token here as well.
        match prover.proof("POST", &endpoint, None).await {
          Ok(proof) => request = request.header(DPOP_HEADER, proof.as_str()),
          Err(error) => tracing::warn!(%error, "no proof for ending the provider session"),
        }
      }
      if let Err(error) = request.form(&form).send().await {
        // The local session is gone either way; the provider session expires on its own.
        tracing::warn!(%error, "could not end the provider session");
      }
    }
    let purged = self
      .store
      .purge_after_sign_out(&server.token_key(), &server.server_url)
      .await;
    local.and(cleared).and(purged)
  }

  /// The provider's prover for ending a bound session; `None` for an unbound one. The key itself
  /// stays: it is per device, and says nothing without a token.
  async fn end_session_prover(&self, server: &DesktopConfig) -> Option<Arc<DpopProver>> {
    let bound = self.stored_binding(server).await.ok()??;
    let prover = self.stored_prover(server).await.ok()??;
    (prover.thumbprint() == bound).then(|| Arc::new(prover.for_another_party()))
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
      .http()
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
      .http()
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
    self.refuse_before_confirmation(path)?;
    // One read of the server: the token and the URL always belong to the same one.
    let server = self.server()?;
    let token = self.access_token(&server).await?;
    let url = format!("{}{path}", server.server_url.trim_end_matches('/'));
    let answer = self
      .send_signed(&server, &token, method, &url, |mut request| {
        request = request.header(reqwest::header::ACCEPT, "application/json");
        if let Some(body) = &maybe_body {
          request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.clone());
        }
        request
      })
      .await?;
    let status = answer.status;
    let body = String::from_utf8_lossy(&answer.body).into_owned();
    if path == ME_PATH
      && status == 200
      && let Ok(me) = serde_json::from_str::<GetMeResponse>(&body)
    {
      *self.confirmed_user() = Some(me.id);
    }
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
    self.refuse_before_confirmation(path)?;
    let server = self.server()?;
    let token = self.access_token(&server).await?;
    let url = format!("{}{path}", server.server_url.trim_end_matches('/'));
    let answer = self
      .send_signed(&server, &token, method, &url, |mut request| {
        if let Some((content_type, bytes)) = &maybe_body {
          request = request
            .header(reqwest::header::CONTENT_TYPE, content_type.clone())
            .body(bytes.clone());
        }
        request
      })
      .await?;
    Ok(ApiBytesResponse {
      status: answer.status,
      content_type: answer.maybe_content_type,
      body_base64: base64::engine::general_purpose::STANDARD.encode(&answer.body),
    })
  }

  /// A request with `token`, signed for a bound one, with what DPoP asks for: the answer's nonce and
  /// clock are kept, and a proof refused for them is made again once, never more (e2ee.md §3.1.2).
  async fn send_signed(
    &self,
    server: &DesktopConfig,
    token: &SecretString,
    method: ApiMethod,
    url: &str,
    build: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
  ) -> Result<SignedAnswer, AuthError> {
    let maybe_prover = match TokenAuthorization::of(token.expose_secret()) {
      TokenAuthorization::Bearer => None,
      TokenAuthorization::Bound(_) => Some(self.stored_prover(server).await?.ok_or(AuthError::KeyLost)?),
    };
    let (first, learned) = self
      .signed_attempt(maybe_prover.as_deref(), token, method, url, &build)
      .await?;
    let is_retried = first.status == 401
      && serde_json::from_slice::<ErrorBody>(&first.body)
        .is_ok_and(|body| learned.is_worth_a_retry(&body.error_code));
    if !is_retried {
      return Ok(first);
    }
    let (second, _) = self
      .signed_attempt(maybe_prover.as_deref(), token, method, url, &build)
      .await?;
    Ok(second)
  }

  async fn signed_attempt(
    &self,
    maybe_prover: Option<&DpopProver>,
    token: &SecretString,
    method: ApiMethod,
    url: &str,
    build: &impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
  ) -> Result<(SignedAnswer, Learned), AuthError> {
    let method = reqwest_method(method);
    let signed = signed_headers(maybe_prover, token.expose_secret(), method.as_str(), url)
      .await
      .map_err(|error| match error {
        DpopSendError::KeyLost => AuthError::KeyLost,
        DpopSendError::Proof(error) => AuthError::Unsupported(error.to_string()),
      })?;
    let mut request = self
      .http()
      .request(method, url)
      .header(AUTHORIZATION, signed.authorization);
    if let Some(proof) = signed.maybe_proof {
      request = request.header(DPOP_HEADER, proof.as_str());
    }
    let response = build(request)
      .send()
      .await
      .map_err(|error| AuthError::Network(error.to_string()))?;
    let learned =
      maybe_prover.map_or_else(Learned::default, |prover| note_answer(prover, response.headers()));
    let status = response.status().as_u16();
    let maybe_content_type = response
      .headers()
      .get(reqwest::header::CONTENT_TYPE)
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned);
    let body = response
      .bytes()
      .await
      .map_err(|error| AuthError::Network(error.to_string()))?
      .to_vec();
    Ok((
      SignedAnswer {
        status,
        maybe_content_type,
        body,
      },
      learned,
    ))
  }

  /// What the core signs its requests with: the key is in memory from the sign-in or refresh that
  /// gave it a token, and a read here touches neither the file nor the keychain.
  pub fn peek_dpop_prover(&self) -> Option<Arc<DpopProver>> {
    self.prover_slot().clone()
  }

  fn prover_slot(&self) -> std::sync::MutexGuard<'_, Option<Arc<DpopProver>>> {
    self.maybe_prover.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// The server's DPoP key, if it has one: from memory, or from the sealed file, which is open
  /// already and costs no keychain access (#1150). A note that doesn't make a key counts as none.
  async fn stored_prover(&self, server: &DesktopConfig) -> Result<Option<Arc<DpopProver>>, AuthError> {
    if let Some(prover) = self.peek_dpop_prover() {
      return Ok(Some(prover));
    }
    let Some(note) = self
      .store
      .load_note(SessionNote::DpopKey, &server.server_url)
      .await?
    else {
      return Ok(None);
    };
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(note);
    let Some(key) = decoded
      .ok()
      .and_then(|secret| DpopSigningKey::from_secret_bytes(&Zeroizing::new(secret)).ok())
    else {
      tracing::warn!("the stored DPoP key can't be used");
      return Ok(None);
    };
    Ok(Some(self.keep_prover(key)))
  }

  /// The server's DPoP key, made and kept on first use.
  async fn prover_or_new(&self, server: &DesktopConfig) -> Result<Arc<DpopProver>, AuthError> {
    if let Some(prover) = self.stored_prover(server).await? {
      return Ok(prover);
    }
    let key = DpopSigningKey::generate();
    let secret = key.to_secret_bytes();
    let note = Zeroizing::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret.as_slice()));
    self
      .store
      .save_note(SessionNote::DpopKey, &server.server_url, &note)
      .await?;
    Ok(self.keep_prover(key))
  }

  fn keep_prover(&self, key: DpopSigningKey) -> Arc<DpopProver> {
    let prover = Arc::new(DpopProver::new(Arc::new(SoftwareDpopSigner::new(key))));
    *self.prover_slot() = Some(Arc::clone(&prover));
    prover
  }

  /// The proof of a token request for a session that isn't bound to a key yet: with one only if the
  /// provider takes them and the server would check them (it offers its nonce on `server-info`).
  /// Otherwise, or when the key can't be kept, the session stays unbound, as it always was: nobody
  /// is locked out by a binding the other side doesn't know (e2ee.md §3.1.2).
  async fn new_session_proof(
    &self,
    session: &mut Session,
    server: &DesktopConfig,
    discovery: &Discovery,
  ) -> TokenProof {
    let takes_proofs = discovery
      .dpop_signing_alg_values_supported
      .iter()
      .any(|algorithm| algorithm == DPOP_ALGORITHM_FOR_DISCOVERY);
    if !takes_proofs {
      return TokenProof::Unbound;
    }
    let Some(nonce) = self.server_nonce(server).await else {
      return TokenProof::Unbound;
    };
    match self.prover_or_new(server).await {
      Ok(prover) => {
        prover.remember_nonce(&nonce);
        TokenProof::With(provider_prover(session, &prover))
      },
      Err(error) => {
        tracing::warn!(%error, "could not keep a DPoP key, the sign-in stays unbound");
        TokenProof::Unbound
      },
    }
  }

  /// The nonce `server-info` offers. None means an older server, a proxy that strips the header, an
  /// address the server doesn't know, or no answer; it is asked again at the next refresh.
  async fn server_nonce(&self, server: &DesktopConfig) -> Option<String> {
    let url = format!(
      "{}{}",
      server.server_url.trim_end_matches('/'),
      server_entry::SERVER_INFO_PATH
    );
    let response = self.http().get(url).send().await.ok()?;
    if !response.status().is_success() {
      return None;
    }
    let nonce = response.headers().get(DPOP_NONCE_HEADER)?.to_str().ok()?;
    Some(nonce.to_owned())
  }

  /// The proof of a refresh: with the key the refresh token is bound to and no other, or, for an
  /// unbound one, as for a new session.
  async fn refresh_proof(
    &self,
    session: &mut Session,
    server: &DesktopConfig,
    discovery: &Discovery,
  ) -> Result<TokenProof, AuthError> {
    let Some(bound) = self.stored_binding(server).await? else {
      return Ok(self.new_session_proof(session, server, discovery).await);
    };
    let Some(prover) = self.stored_prover(server).await? else {
      return Err(AuthError::KeyLost);
    };
    if prover.thumbprint() != bound {
      return Err(AuthError::KeyLost);
    }
    Ok(TokenProof::With(provider_prover(session, &prover)))
  }

  /// The key the stored refresh token is bound to.
  async fn stored_binding(&self, server: &DesktopConfig) -> Result<Option<JwkThumbprint>, AuthError> {
    let maybe_note = self
      .store
      .load_note(SessionNote::TokenBinding, &server.token_key())
      .await?;
    Ok(maybe_note.and_then(|note| JwkThumbprint::parse(&note).ok()))
  }

  /// Keeps what a token answer says about its binding, for the next refresh of the refresh token.
  async fn note_binding(&self, server: &DesktopConfig, access: &str) {
    let key = server.token_key();
    let saved = match token_bound_key(access) {
      Some(bound) => {
        self
          .store
          .save_note(SessionNote::TokenBinding, &key, bound.as_str())
          .await
      },
      None => self.store.clear_note(SessionNote::TokenBinding, &key).await,
    };
    if let Err(error) = saved {
      tracing::warn!(%error, "could not keep the sign-in's binding");
    }
  }

  /// The current access token as text, refreshed when needed (tests; the app uses
  /// [`Self::server_bearer_token`]).
  #[cfg(test)]
  pub async fn bearer_token(&self) -> Result<String, AuthError> {
    Ok(self.server_bearer_token().await?.1)
  }

  /// Nothing but the question goes out before the server confirmed the cached account (the UI waits
  /// a moment before it gets here, `auth/account_gate.rs`).
  fn refuse_before_confirmation(&self, path: &str) -> Result<(), AuthError> {
    if self.is_unconfirmed() && path != ME_PATH {
      return Err(AuthError::Network(
        "waiting for the server to confirm the account".to_owned(),
      ));
    }
    Ok(())
  }

  /// `(server origin, access token)`: the token with the server it is for.
  pub async fn server_bearer_token(&self) -> Result<(String, String), AuthError> {
    self.refuse_before_confirmation("")?;
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
    // Started from the cached account: the core waits for the server to confirm it, whatever the
    // session holds by now. A wait, not a sign-out.
    if self.is_unconfirmed() {
      return TokenPeek::Refreshing;
    }
    let Ok(session) = self.session.try_lock() else {
      return TokenPeek::Busy;
    };
    let maybe_token = match (&session.maybe_local, &session.maybe_tokens) {
      (Some(local), _) => local
        .remaining(local::now_ms())
        .map(|valid_for| (local.token(), valid_for)),
      (None, Some(tokens)) => {
        let valid_for = tokens.remaining((self.now_ms)());
        if valid_for <= TOKEN_EXPIRY_MARGIN && tokens.refresh.is_some() {
          return TokenPeek::Refreshing;
        }
        (valid_for > TOKEN_EXPIRY_MARGIN).then(|| {
          (
            SecretString::from(tokens.access.expose_secret().to_owned()),
            valid_for - TOKEN_EXPIRY_MARGIN,
          )
        })
      },
      (None, None) if session.is_key_lost => return TokenPeek::KeyLost,
      (None, None) if session.is_expired => return TokenPeek::Expired,
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
    self
      .store
      .clear_note(SessionNote::CachedAccount, &server.server_url)
      .await?;
    self.store.clear(&server.token_key()).await?;
    self.store.clear_local_session(&server.server_url).await?;
    self
      .store
      .purge_after_sign_out(&server.token_key(), &server.server_url)
      .await
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
      // A refused refresh token is no wait: the session is over, and the next look says so.
      None if self.is_unconfirmed() && !session.is_expired => {
        return Err(AuthError::Network("waiting for the stored sign-in".to_owned()));
      },
      None => return Err(AuthError::NotSignedIn),
      Some(tokens) => (self.now_ms)() >= tokens.refresh_at_ms,
    };
    if needs_refresh {
      self.refresh_session(&mut session, server).await?;
    }
    session
      .maybe_tokens
      .as_ref()
      .map(|tokens| SecretString::from(tokens.access.expose_secret().to_owned()))
      .ok_or(AuthError::NotSignedIn)
  }

  /// The server refused the access token with fingerprint `refused` ([`token_fingerprint`]):
  /// refreshes once, unless another request already did (single flight: the session lock
  /// serialises them, and a refusal of an older token only retries). A token that was itself
  /// fetched after a refusal is not refreshed again: the server's answer stands, so a server that
  /// refuses everything can't make the app ask the provider on every request. `Ok(true)` means a
  /// newer token is there and the request is worth one more try; the caller retries once, never
  /// in a loop (issue #883).
  ///
  /// Only a refused refresh token ([`AuthError::Denied`]) ends the session; a network failure
  /// keeps it for the next try.
  pub async fn refresh_rejected(&self, refused: u64) -> Result<bool, AuthError> {
    let server = self.server()?;
    let mut session = self.session.lock().await;
    if self.active_server().as_ref() != Some(&server) {
      return Err(AuthError::NotSignedIn);
    }
    // A local account has no refresh token: the server's answer stands.
    let Some(tokens) = &session.maybe_tokens else {
      return Ok(false);
    };
    if token_fingerprint(tokens.access.expose_secret()) != refused {
      return Ok(true);
    }
    if tokens.is_from_refusal {
      return Ok(false);
    }
    self.refresh_session(&mut session, &server).await?;
    if let Some(tokens) = session.maybe_tokens.as_mut() {
      tokens.is_from_refusal = true;
    }
    Ok(true)
  }

  /// Refreshes the session's tokens with its refresh token; a refused one ends the session.
  async fn refresh_session(&self, session: &mut Session, server: &DesktopConfig) -> Result<(), AuthError> {
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
    let result = self.refresh_with(session, server, refresh).await;
    if let Err(error) = &result {
      tracing::warn!(kind = error.kind(), "token refresh failed");
      if matches!(error, AuthError::Denied(_) | AuthError::KeyLost) {
        // Only the provider's refusal, or a key that is gone, ends the session; no answer keeps it
        // (issue #883). The stored token goes too, so the next start doesn't offer it again.
        session.maybe_tokens = None;
        session.is_expired = true;
        session.is_key_lost = matches!(error, AuthError::KeyLost);
        if let Err(error) = self.store.clear(&server.token_key()).await {
          tracing::warn!(%error, "could not forget the refused refresh token");
        }
        if let Err(error) = self
          .store
          .clear_note(SessionNote::CachedAccount, &server.server_url)
          .await
        {
          tracing::warn!(%error, "could not forget the cached account");
        }
      }
    }
    result
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
    let token_proof = self.refresh_proof(session, server, &discovery).await?;
    let http = self.oauth_http(&token_proof);
    let refresh_token = RefreshToken::new(refresh.expose_secret().to_owned());
    let refreshing = client.exchange_refresh_token(&refresh_token);
    let exchange = refreshing.request_async(&http);
    // Next to the refresh, not after it: the nonce saves the first request after a start its
    // challenge, for no extra wait.
    let (response, maybe_nonce) = tokio::join!(exchange, async {
      match &token_proof {
        TokenProof::With(_) => self.server_nonce(server).await,
        TokenProof::Unbound => None,
      }
    });
    if let (Some(nonce), Some(prover)) = (maybe_nonce, self.peek_dpop_prover()) {
      prover.remember_nonce(&nonce);
    }
    let response = response.map_err(map_token_error)?;
    self
      .adopt(session, server, &response, TokenOrigin::SameSession)
      .await
  }

  /// Who the tokens of `response` belong to, to be saved once they are in. What is stored about the
  /// previous ones goes first, and a failure stops the sign-in like a failure to save the token: a
  /// cached account of somebody else can't match the new tokens, whatever else fails from here on.
  async fn prepare_subject(
    &self,
    server: &DesktopConfig,
    response: &OidcTokenResponse,
    origin: TokenOrigin,
  ) -> Result<Option<String>, AuthError> {
    let key = server.token_key();
    let maybe_answered = answer_subject(
      response.extra_fields().maybe_id_token.as_deref(),
      response.access_token().secret(),
    );
    let is_stale = match (origin, &maybe_answered) {
      (TokenOrigin::NewSignIn, _) => true,
      // A refresh answer that names nobody says nothing against who the session belongs to.
      (TokenOrigin::SameSession, None) => false,
      (TokenOrigin::SameSession, Some(answered)) => {
        self
          .store
          .load_note(SessionNote::TokenSubject, &key)
          .await?
          .as_ref()
          != Some(answered)
      },
    };
    if is_stale {
      self.store.clear_note(SessionNote::TokenSubject, &key).await?;
    }
    Ok(maybe_answered)
  }

  /// Takes over a token response of `server`: keeps the access token in memory and persists the
  /// refresh token. A response that arrives after the user switched servers is dropped.
  async fn adopt(
    &self,
    session: &mut Session,
    server: &DesktopConfig,
    response: &OidcTokenResponse,
    origin: TokenOrigin,
  ) -> Result<(), AuthError> {
    if self.active_server().as_ref() != Some(server) {
      return Err(AuthError::Cancelled);
    }
    let maybe_subject = self.prepare_subject(server, response, origin).await?;
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
    self.note_binding(server, response.access_token().secret()).await;
    if let Some(subject) = maybe_subject {
      // Not saved: no cached start until a refresh saves it. It was cleared before the token went in.
      if let Err(error) = self
        .store
        .save_note(SessionNote::TokenSubject, &server.token_key(), &subject)
        .await
      {
        tracing::warn!(%error, "could not keep who the sign-in belongs to");
      }
    }
    if origin == TokenOrigin::NewSignIn {
      self.is_unconfirmed.store(false, Ordering::Release);
    }
    let lifetime = response.expires_in().unwrap_or(Duration::from_secs(60));
    let now_ms = (self.now_ms)();
    let ms = |duration: Duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
    let expires_at_ms = now_ms.saturating_add(ms(lifetime));
    session.is_expired = false;
    session.is_key_lost = false;
    session.maybe_tokens = Some(Tokens {
      access: SecretString::from(response.access_token().secret().clone()),
      refresh: refresh.map(SecretString::from),
      expires_at_ms,
      refresh_at_ms: expires_at_ms.saturating_sub(ms(REFRESH_MARGIN.min(lifetime / 2))),
      is_from_refusal: false,
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
    session.is_expired = false;
    self.is_unconfirmed.store(false, Ordering::Release);
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
    let cleared = self.store.clear_local_session(&server.server_url).await;
    let Some(session) = maybe_session.filter(|session| session.is_live(local::now_ms())) else {
      return cleared;
    };
    let url = format!("{}{LOCAL_LOGOUT_PATH}", server.server_url.trim_end_matches('/'));
    if let Err(error) = self
      .http()
      .post(url)
      .bearer_auth(session.access.expose_secret())
      .send()
      .await
    {
      // The local session is gone either way; the server's token expires on its own.
      tracing::warn!(%error, "could not end the local session on the server");
    }
    cleared
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
      .http()
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

  /// The HTTP client of the next request (a cheap handle on the shared pool).
  fn http(&self) -> reqwest::Client {
    self.http.read().unwrap_or_else(PoisonError::into_inner).clone()
  }

  /// HTTP client of the `oauth2` crate on top of our `reqwest` client, with `proof`'s DPoP proofs.
  fn oauth_http(&self, proof: &TokenProof) -> OAuthHttp {
    OAuthHttp {
      http: self.http(),
      maybe_prover: match proof {
        TokenProof::Unbound => None,
        TokenProof::With(prover) => Some(Arc::clone(prover)),
      },
    }
  }
}

fn http_client() -> Result<reqwest::Client, AuthError> {
  reqwest::Client::builder()
    .timeout(HTTP_TIMEOUT)
    // A redirect would carry the bearer token to wherever a hostile answer points.
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .map_err(|error| AuthError::Unsupported(error.to_string()))
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

/// One authorization request of a browser sign-in, waiting for its redirect.
struct Authorization {
  redirect_uri: String,
  client: OidcClient,
  verifier: PkceCodeVerifier,
  state: CsrfToken,
  url: oauth2::url::Url,
}

/// What the browser tab says about a finished sign-in.
fn callback_outcome<T>(outcome: &Result<T, AuthError>) -> CallbackOutcome {
  match outcome {
    Ok(_) => CallbackOutcome::SignedIn,
    Err(AuthError::Denied(_) | AuthError::AuthorizationRefused(_) | AuthError::Cancelled) => {
      CallbackOutcome::Refused
    },
    Err(AuthError::StateMismatch) => CallbackOutcome::StateMismatch,
    Err(_) => CallbackOutcome::Failed,
  }
}

/// Adapter that lets the `oauth2` crate send its token requests through our `reqwest` client.
struct OAuthHttp {
  http: reqwest::Client,
  maybe_prover: Option<Arc<DpopProver>>,
}

impl<'c> oauth2::AsyncHttpClient<'c> for OAuthHttp {
  type Error = OAuthHttpError;
  type Future = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<oauth2::HttpResponse, OAuthHttpError>> + Send + 'c>,
  >;

  fn call(&'c self, request: oauth2::HttpRequest) -> Self::Future {
    Box::pin(async move {
      let (parts, body) = request.into_parts();
      let url = parts.uri.to_string();
      let attempt = |maybe_proof: Option<String>| {
        let mut request = self
          .http
          .request(parts.method.clone(), &url)
          .headers(parts.headers.clone())
          .body(body.clone());
        if let Some(proof) = maybe_proof {
          request = request.header(DPOP_HEADER, proof);
        }
        send_oauth(request)
      };
      match &self.maybe_prover {
        Some(prover) => with_token_endpoint_proof(prover, &url, |proof| attempt(Some(proof))).await,
        None => attempt(None).await.map(|answer| answer.response),
      }
    })
  }
}

/// One request of the `oauth2` crate.
async fn send_oauth(
  request: reqwest::RequestBuilder,
) -> Result<TokenEndpointAttempt<oauth2::HttpResponse>, OAuthHttpError> {
  let response = request
    .send()
    .await
    .map_err(|error| OAuthHttpError(error.to_string()))?;
  let status = response.status();
  let header = |name: &str| {
    response
      .headers()
      .get(name)
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned)
  };
  let (maybe_nonce, maybe_date) = (header(DPOP_NONCE_HEADER), header(DATE.as_str()));
  let mut builder = oauth2::http::Response::builder().status(status);
  for (name, value) in response.headers() {
    builder = builder.header(name, value);
  }
  let bytes = response
    .bytes()
    .await
    .map_err(|error| OAuthHttpError(error.to_string()))?;
  let answer = builder
    .body(bytes.to_vec())
    .map_err(|error| OAuthHttpError(error.to_string()))?;
  Ok(TokenEndpointAttempt {
    status: status.as_u16(),
    body: bytes.to_vec(),
    maybe_nonce,
    maybe_date,
    response: answer,
  })
}

/// Takes in what an API answer says for the next proof.
fn note_answer(prover: &DpopProver, headers: &HeaderMap) -> Learned {
  let text = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
  prover.note_answer(DpopAnswer {
    maybe_nonce: text(DPOP_NONCE_HEADER),
    maybe_date: text(DATE.as_str()),
  })
}

/// The identity provider's prover of `prover`'s key, kept for the session: it knows the provider's
/// nonce.
fn provider_prover(session: &mut Session, prover: &DpopProver) -> Arc<DpopProver> {
  match &session.maybe_provider_prover {
    Some(known) if known.thumbprint() == prover.thumbprint() => Arc::clone(known),
    _ => {
      let made = Arc::new(prover.for_another_party());
      session.maybe_provider_prover = Some(Arc::clone(&made));
      made
    },
  }
}

impl From<DpopProofError> for OAuthHttpError {
  fn from(error: DpopProofError) -> Self {
    Self(error.to_string())
  }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct OAuthHttpError(String);

type TokenError = RequestTokenError<OAuthHttpError, StandardErrorResponse<BasicErrorResponseType>>;

/// Only `invalid_grant` (the refresh token expired, was revoked or reused) ends the session
/// (issue #667); any other answer of the provider (`temporarily_unavailable`, `server_error`, a
/// misconfigured client) keeps the refresh token for the next try.
fn map_token_error(error: TokenError) -> AuthError {
  match error {
    RequestTokenError::ServerResponse(response) => {
      let description = response
        .error_description()
        .cloned()
        .unwrap_or_else(|| response.error().to_string());
      if *response.error() == BasicErrorResponseType::InvalidGrant {
        AuthError::Denied(description)
      } else {
        AuthError::InvalidResponse(description)
      }
    },
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

  #[test]
  fn only_a_refused_refresh_token_ends_the_session() {
    let answer = |kind| {
      map_token_error(RequestTokenError::ServerResponse(StandardErrorResponse::new(
        kind, None, None,
      )))
    };
    assert!(matches!(
      answer(BasicErrorResponseType::InvalidGrant),
      AuthError::Denied(_)
    ));
    for kept in [
      BasicErrorResponseType::Extension("temporarily_unavailable".into()),
      BasicErrorResponseType::Extension("server_error".into()),
      BasicErrorResponseType::InvalidClient,
    ] {
      assert!(!matches!(answer(kept), AuthError::Denied(_)));
    }
  }

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

  /// Issue #883: tokens that run out while the computer sleeps, and the refresh after a refusal.
  mod token_expiry {
    use std::sync::atomic::{AtomicI64, AtomicU8, AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use teamcraft_core::store::{KeySourceError, Keystore, SecretVault};
    use zeroize::Zeroizing;

    use super::*;
    use crate::auth::store::MemoryStore;

    const START_MS: i64 = 1_800_000_000_000;
    const LIFETIME_MS: i64 = 300_000;
    const HOUR_MS: i64 = 3_600_000;
    /// The fake identity provider's token endpoint: no answer, a token, or `invalid_grant`.
    const PROVIDER_DOWN: u8 = 0;
    const PROVIDER_UP: u8 = 1;
    const PROVIDER_REFUSES: u8 = 2;
    /// Whom the fake provider's tokens name, and who the fake server says that is.
    const TOKEN_SUBJECT: &str = "subject-of-the-provider";
    const ME_ANSWER: &str = r#"{"id":"42","display_name":"Alice","role":"owner"}"#;

    /// The computer is off for 12 hours, 3 days or 30 days; it wakes up without network. The access
    /// token is long expired: the app waits (offline, never "sign in again") until the provider can
    /// be reached and then refreshes with the stored refresh token.
    #[tokio::test]
    async fn after_a_long_time_off_the_refresh_waits_for_the_network_and_succeeds() {
      for off_ms in [12 * HOUR_MS, 3 * 24 * HOUR_MS, 30 * 24 * HOUR_MS] {
        let provider = FakeProvider::start().await;
        let (service, clock) = signed_in(provider.config());
        service
          .store
          .save(&provider.config().token_key(), "refresh")
          .await
          .unwrap();
        clock.fetch_add(off_ms, Ordering::SeqCst);

        for _ in 0..3 {
          assert!(matches!(service.bearer_token().await, Err(AuthError::Network(_))));
          assert!(
            matches!(service.peek_server_bearer_token(), TokenPeek::Refreshing),
            "{off_ms} ms off: no answer is never a refusal"
          );
        }
        assert!(
          service
            .store
            .load(&provider.config().token_key())
            .await
            .unwrap()
            .is_some()
        );

        provider.set(PROVIDER_UP);
        assert_eq!(service.bearer_token().await.unwrap(), "fresh-access");
        assert!(matches!(
          service.peek_server_bearer_token(),
          TokenPeek::Ready { .. }
        ));
        assert_eq!(
          service
            .store
            .load(&provider.config().token_key())
            .await
            .unwrap()
            .map(|token| token.expose_secret().to_owned())
            .as_deref(),
          Some("rotated-refresh"),
          "the rotated refresh token is kept"
        );
      }
    }

    /// Only the provider's own refusal ends the session, and then once: the token is forgotten and
    /// the session says it expired (the UI goes back to the sign-in screen with that message).
    #[tokio::test]
    async fn a_refused_refresh_token_ends_the_session_once() {
      let provider = FakeProvider::start().await;
      let (service, clock) = signed_in(provider.config());
      service
        .store
        .save(&provider.config().token_key(), "refresh")
        .await
        .unwrap();
      clock.fetch_add(3 * 24 * HOUR_MS, Ordering::SeqCst);
      provider.set(PROVIDER_REFUSES);

      assert!(matches!(service.bearer_token().await, Err(AuthError::Denied(_))));
      assert!(matches!(service.peek_server_bearer_token(), TokenPeek::Expired));
      assert!(
        service
          .store
          .load(&provider.config().token_key())
          .await
          .unwrap()
          .is_none()
      );
      let asked = provider.token_requests();
      assert!(matches!(
        service.bearer_token().await,
        Err(AuthError::NotSignedIn)
      ));
      assert_eq!(
        provider.token_requests(),
        asked,
        "a refused token is not tried again"
      );
    }

    /// Issue #1150: on macOS every keychain call can raise a dialog, and the token rotates every
    /// few minutes. A start reads the keychain once; refreshing never touches it.
    #[tokio::test]
    async fn a_start_reads_the_keychain_once_and_a_refresh_not_at_all() {
      let provider = FakeProvider::start().await;
      provider.set(PROVIDER_UP);
      let keystore = Arc::new(CountingKeystore::default());
      let directory = std::env::temp_dir().join(format!("teamcraft-auth-vault-{}", std::process::id()));
      let open_vault = || {
        let vault = Arc::new(SecretVault::new(Arc::clone(&keystore) as Arc<dyn Keystore>));
        vault.set_directory(&directory);
        vault
      };
      let first_run = VaultStore::new(open_vault());
      first_run
        .save(&provider.config().token_key(), "refresh")
        .await
        .unwrap();
      first_run
        .clear_local_session(&provider.config().server_url)
        .await
        .unwrap();
      drop(first_run);
      keystore.calls.store(0, Ordering::SeqCst);

      let clock = Arc::new(AtomicI64::new(START_MS));
      let reading = Arc::clone(&clock);
      let service = AuthService::with_clock(
        Some(provider.config()),
        VaultStore::new(open_vault()),
        Arc::new(move || reading.load(Ordering::SeqCst)),
      );
      assert!(service.restore().await.unwrap());
      assert_eq!(keystore.calls.load(Ordering::SeqCst), 1, "one read per start");

      let requests = provider.token_requests.load(Ordering::SeqCst);
      keystore.calls.store(0, Ordering::SeqCst);
      for round in 1..=5 {
        clock.store(START_MS + round * 10 * HOUR_MS, Ordering::SeqCst);
        service.access_token(&provider.config()).await.unwrap();
      }
      assert!(
        provider.token_requests.load(Ordering::SeqCst) > requests,
        "it did refresh"
      );
      assert_eq!(
        keystore.calls.load(Ordering::SeqCst),
        0,
        "no access while running"
      );
      drop(std::fs::remove_dir_all(&directory));
    }

    /// The stored token could not be cleared (a full disk): the sign-out says so, and the provider
    /// still ends the session, because the token is taken and a retry would have none to send.
    #[tokio::test]
    async fn a_failed_local_clear_still_ends_the_provider_session() {
      let provider = FakeProvider::start().await;
      provider.set(PROVIDER_UP);
      let store = RefusesClear::default();
      store
        .0
        .save(&provider.config().token_key(), "refresh")
        .await
        .unwrap();
      let service = AuthService::new(Some(provider.config()), store).unwrap();
      assert!(service.restore().await.unwrap());

      assert!(service.logout().await.is_err());

      assert_eq!(provider.logout_requests.load(Ordering::SeqCst), 1);
    }

    mod cached_account {
      use super::*;

      const SUBJECT_A: &str = "subject-a";

      fn account() -> GetMeResponse {
        serde_json::from_str(r#"{"id":"42","display_name":"Alice","role":"owner"}"#).unwrap()
      }

      /// A service that holds a refresh token and the account confirmed last, like at a start.
      async fn starting(provider: &FakeProvider) -> AuthService<MemoryStore> {
        let store = MemoryStore::default();
        let config = provider.config();
        store.save(&config.token_key(), "refresh").await.unwrap();
        store
          .save_note(SessionNote::TokenSubject, &config.token_key(), SUBJECT_A)
          .await
          .unwrap();
        let record = CachedAccountRecord {
          subject: SUBJECT_A.to_owned(),
          account: account(),
        };
        store
          .save_note(
            SessionNote::CachedAccount,
            &config.server_url,
            &serde_json::to_string(&record).unwrap(),
          )
          .await
          .unwrap();
        AuthService::new(Some(config), store).unwrap()
      }

      async fn stored_record(service: &AuthService<MemoryStore>, provider: &FakeProvider) -> Option<String> {
        service
          .store
          .load_note(SessionNote::CachedAccount, &provider.config().server_url)
          .await
          .unwrap()
      }

      /// A token answer naming `subject`, or nobody.
      fn token_answer(maybe_subject: Option<&str>) -> OidcTokenResponse {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let access = match maybe_subject {
          Some(subject) => {
            let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"sub":"{subject}"}}"#));
            format!("eyJhbGciOiJub25lIn0.{payload}.signature")
          },
          None => "opaque".to_owned(),
        };
        serde_json::from_value(serde_json::json!({
          "access_token": access,
          "token_type": "bearer",
          "expires_in": 300,
          "refresh_token": "new-refresh",
        }))
        .unwrap()
      }

      async fn adopt(
        service: &AuthService<MemoryStore>,
        provider: &FakeProvider,
        maybe_subject: Option<&str>,
        origin: TokenOrigin,
      ) {
        let mut session = service.session.lock().await;
        service
          .adopt(
            &mut session,
            &provider.config(),
            &token_answer(maybe_subject),
            origin,
          )
          .await
          .unwrap();
      }

      #[tokio::test]
      async fn it_needs_no_network_and_tells_the_core_to_wait() {
        let provider = FakeProvider::start().await;
        provider.set(PROVIDER_DOWN);
        let service = starting(&provider).await;

        assert_eq!(service.cached_account().await, Some(account()));
        assert_eq!(provider.token_requests.load(Ordering::SeqCst), 0);
        assert!(matches!(
          service.peek_server_bearer_token(),
          TokenPeek::Refreshing
        ));
        // A request the UI makes meanwhile hears "no network", not "signed out".
        assert!(matches!(service.bearer_token().await, Err(AuthError::Network(_))));
      }

      #[tokio::test]
      async fn a_session_that_is_gone_takes_it_along() {
        let provider = FakeProvider::start().await;
        let service = starting(&provider).await;
        service.store.clear(&provider.config().token_key()).await.unwrap();
        assert_eq!(service.cached_account().await, None);
        assert!(!service.is_unconfirmed());
      }

      #[tokio::test]
      async fn no_answer_from_the_provider_keeps_it() {
        let provider = FakeProvider::start().await;
        provider.set(PROVIDER_DOWN);
        let service = starting(&provider).await;
        service.cached_account().await.unwrap();

        assert!(service.restore().await.is_err());

        assert!(service.is_unconfirmed());
        assert_eq!(service.cached_account().await, Some(account()));
      }

      #[tokio::test]
      async fn a_refused_refresh_token_forgets_it_and_ends_the_wait() {
        let provider = FakeProvider::start().await;
        provider.set(PROVIDER_REFUSES);
        let service = starting(&provider).await;
        service.cached_account().await.unwrap();

        assert!(matches!(service.restore().await, Err(AuthError::Denied(_))));

        assert!(!service.is_unconfirmed());
        assert_eq!(stored_record(&service, &provider).await, None);
      }

      /// The session being fine says nothing about whose it is: the core has no token, and nothing
      /// goes out but the question, until the server confirmed the account.
      #[tokio::test]
      async fn only_the_confirmation_gives_the_core_its_token() {
        let provider = FakeProvider::start().await;
        provider.set(PROVIDER_UP);
        let service = starting(&provider).await;
        service.cached_account().await.unwrap();

        assert!(service.restore().await.unwrap());

        assert!(service.is_unconfirmed());
        assert!(matches!(
          service.peek_server_bearer_token(),
          TokenPeek::Refreshing
        ));
        assert!(matches!(
          service
            .api_request(ApiMethod::Get, "/api/v1/conversations", None)
            .await,
          Err(AuthError::Network(_))
        ));
        assert!(matches!(service.bearer_token().await, Err(AuthError::Network(_))));

        *service.confirmed_user() = Some(account().id);
        service.remember_account(&account()).await;

        assert!(!service.is_unconfirmed());
        assert!(matches!(
          service.peek_server_bearer_token(),
          TokenPeek::Ready { .. }
        ));
      }

      #[tokio::test]
      async fn signing_out_forgets_it_and_a_late_answer_brings_nothing_back() {
        let provider = FakeProvider::start().await;
        provider.set(PROVIDER_UP);
        let service = starting(&provider).await;
        service.cached_account().await.unwrap();
        *service.confirmed_user() = Some(account().id);

        service.logout().await.unwrap();

        assert!(!service.is_unconfirmed());
        assert_eq!(service.cached_account().await, None);
        service.remember_account(&account()).await;
        assert_eq!(stored_record(&service, &provider).await, None);
      }

      /// The fake provider's config, with the fake server at the same address.
      fn config_with_server(provider: &FakeProvider) -> DesktopConfig {
        DesktopConfig {
          server_url: provider.base.clone(),
          ..provider.config()
        }
      }

      /// The whole way: a start without a cached account, the UI's `GET /me`, the UI hands the account
      /// over; the next start begins from it. Without anything set by hand.
      #[tokio::test]
      async fn the_next_start_begins_from_what_the_server_confirmed() {
        let provider = FakeProvider::start().await;
        provider.set(PROVIDER_UP);
        let config = config_with_server(&provider);
        let store = MemoryStore::default();
        store.save(&config.token_key(), "refresh").await.unwrap();
        let service = AuthService::new(Some(config), store).unwrap();
        assert_eq!(service.cached_account().await, None);

        assert!(service.restore().await.unwrap());
        let answer = service.api_get(ME_PATH).await.unwrap();
        let me: GetMeResponse = serde_json::from_str(&answer.body).unwrap();
        service.remember_account(&me).await;

        assert_eq!(service.cached_account().await, Some(me));
        assert!(service.is_unconfirmed());
      }

      /// The UI hands over somebody the server didn't name: not kept.
      #[tokio::test]
      async fn only_who_the_server_named_is_kept() {
        let provider = FakeProvider::start().await;
        provider.set(PROVIDER_UP);
        let config = config_with_server(&provider);
        let store = MemoryStore::default();
        store.save(&config.token_key(), "refresh").await.unwrap();
        let service = AuthService::new(Some(config), store).unwrap();
        assert!(service.restore().await.unwrap());
        service.api_get(ME_PATH).await.unwrap();

        let mut someone_else = account();
        someone_else.id = serde_json::from_str(r#""43""#).unwrap();
        service.remember_account(&someone_else).await;

        assert_eq!(service.cached_account().await, None);
      }

      /// Restored, then no answer from the server, then the token needs a refresh and the provider
      /// refuses it. The UI hears that, and a look after that doesn't read as a network problem.
      #[tokio::test]
      async fn a_refresh_refused_while_unconfirmed_says_so() {
        let provider = FakeProvider::start().await;
        provider.set(PROVIDER_UP);
        let config = config_with_server(&provider);
        let store = MemoryStore::default();
        store.save(&config.token_key(), "refresh").await.unwrap();
        let clock = Arc::new(AtomicI64::new(START_MS));
        let reading = Arc::clone(&clock);
        let service = AuthService::with_clock(
          Some(config.clone()),
          store,
          Arc::new(move || reading.load(Ordering::SeqCst)),
        );
        assert!(service.restore().await.unwrap());
        service.is_unconfirmed.store(true, Ordering::Release);

        clock.store(START_MS + HOUR_MS, Ordering::SeqCst);
        provider.set(PROVIDER_REFUSES);

        assert!(matches!(
          service.api_get(ME_PATH).await,
          Err(AuthError::Denied(_))
        ));
        assert!(matches!(
          service.api_get(ME_PATH).await,
          Err(AuthError::NotSignedIn)
        ));
        assert!(service.store.load(&config.token_key()).await.unwrap().is_none());
      }

      /// Somebody else signed in, and writing could not even forget the old record: it still can't
      /// open for the new tokens.
      #[tokio::test]
      async fn a_record_of_another_subject_does_not_open() {
        let provider = FakeProvider::start().await;
        let service = starting(&provider).await;
        adopt(&service, &provider, Some("subject-b"), TokenOrigin::NewSignIn).await;

        assert!(
          stored_record(&service, &provider).await.is_some(),
          "the old record is still there"
        );
        assert_eq!(service.cached_account().await, None);
        assert!(!service.is_unconfirmed());
      }

      /// The answer for the previous account arrives after somebody else signed in.
      #[tokio::test]
      async fn a_late_confirmation_is_not_kept_for_the_new_session() {
        let provider = FakeProvider::start().await;
        let service = starting(&provider).await;
        *service.confirmed_user() = Some(account().id);
        // What `login` does before the browser opens.
        *service.confirmed_user() = None;
        service.forget_cached_account().await;
        adopt(&service, &provider, Some("subject-b"), TokenOrigin::NewSignIn).await;

        service.remember_account(&account()).await;

        assert_eq!(stored_record(&service, &provider).await, None);
      }

      #[tokio::test]
      async fn a_sign_in_that_names_nobody_gets_no_cached_start() {
        let provider = FakeProvider::start().await;
        let service = starting(&provider).await;
        adopt(&service, &provider, None, TokenOrigin::NewSignIn).await;
        assert_eq!(service.cached_account().await, None);
      }

      /// A refresh keeps who the session belongs to, also when its answer names nobody.
      #[tokio::test]
      async fn a_refresh_keeps_it_unless_it_names_somebody_else() {
        let provider = FakeProvider::start().await;
        let service = starting(&provider).await;
        adopt(&service, &provider, None, TokenOrigin::SameSession).await;
        assert_eq!(service.cached_account().await, Some(account()));
        adopt(&service, &provider, Some(SUBJECT_A), TokenOrigin::SameSession).await;
        assert_eq!(service.cached_account().await, Some(account()));
        adopt(&service, &provider, Some("subject-b"), TokenOrigin::SameSession).await;
        assert_eq!(service.cached_account().await, None);
      }

      /// A new sign-in goes first: a failure to write stops it rather than leave the old subject.
      #[tokio::test]
      async fn a_new_sign_in_forgets_it_first() {
        let provider = FakeProvider::start().await;
        provider.set(PROVIDER_DOWN);
        let service = starting(&provider).await;

        let outcome = service
          .login_local("someone@example.com", &SecretString::from("password"), None)
          .await;

        assert!(outcome.is_err());
        assert_eq!(stored_record(&service, &provider).await, None);
      }

      #[tokio::test]
      async fn accounts_of_different_servers_stay_apart() {
        let provider = FakeProvider::start().await;
        let service = starting(&provider).await;
        assert_eq!(
          service
            .store
            .load_note(SessionNote::CachedAccount, "https://other.example.com")
            .await
            .unwrap(),
          None
        );
      }
    }

    #[tokio::test]
    async fn a_refused_stored_token_at_start_says_the_session_expired() {
      let provider = FakeProvider::start().await;
      provider.set(PROVIDER_REFUSES);
      let store = MemoryStore::default();
      store
        .save(&provider.config().token_key(), "refresh")
        .await
        .unwrap();
      let service = AuthService::new(Some(provider.config()), store).unwrap();
      assert!(matches!(service.restore().await, Err(AuthError::Denied(_))));
      assert!(
        service
          .store
          .load(&provider.config().token_key())
          .await
          .unwrap()
          .is_none()
      );
      assert!(!service.restore().await.unwrap(), "nothing stored any more");
    }

    #[tokio::test]
    async fn a_token_that_ran_out_during_a_suspend_is_not_handed_out() {
      let (service, clock) = signed_in(config());
      assert!(matches!(
        service.peek_server_bearer_token(),
        TokenPeek::Ready { .. }
      ));
      // A ten-minute suspend: the wall clock moves on, a monotonic one would not have.
      clock.fetch_add(2 * LIFETIME_MS, Ordering::SeqCst);
      assert!(
        matches!(service.peek_server_bearer_token(), TokenPeek::Refreshing),
        "waiting for the refresh, not signed out"
      );
    }

    #[tokio::test]
    async fn a_refusal_of_an_older_token_only_retries() {
      let (service, _clock) = signed_in(config());
      // Another request has refreshed already: retry without asking the provider again.
      assert!(
        service
          .refresh_rejected(token_fingerprint("older"))
          .await
          .unwrap()
      );
    }

    /// A server that refuses every token gets one refresh, not one per request.
    #[tokio::test]
    async fn a_refused_fresh_token_is_not_refreshed_again() {
      let provider = FakeProvider::start().await;
      provider.set(PROVIDER_UP);
      let (service, _clock) = signed_in(provider.config());
      assert!(
        service
          .refresh_rejected(token_fingerprint("access"))
          .await
          .unwrap()
      );
      assert_eq!(provider.token_requests(), 1);
      let fresh = token_fingerprint("fresh-access");
      for _ in 0..5 {
        assert!(
          !service.refresh_rejected(fresh).await.unwrap(),
          "the server's answer stands"
        );
      }
      assert_eq!(provider.token_requests(), 1);
    }

    #[tokio::test]
    async fn no_answer_from_the_provider_keeps_the_session() {
      let mut unreachable = config();
      // Nothing listens on port 1: the refresh gets no answer (like a suspend without network).
      unreachable.issuer = "http://127.0.0.1:1/realms/company".into();
      let (service, clock) = signed_in(unreachable);
      clock.fetch_add(2 * LIFETIME_MS, Ordering::SeqCst);
      assert!(matches!(
        service.refresh_rejected(token_fingerprint("access")).await,
        Err(AuthError::Network(_))
      ));
      assert!(matches!(service.bearer_token().await, Err(AuthError::Network(_))));
      assert!(service.is_signed_in().await, "no answer is not a refusal");
    }

    #[tokio::test]
    async fn a_local_account_has_nothing_to_refresh() {
      let service = AuthService::new(Some(config()), MemoryStore::default()).unwrap();
      service
        .adopt_local(
          &config(),
          LocalSession {
            access: SecretString::from("local".to_owned()),
            expires_at_ms: local::now_ms() + 3_600_000,
          },
        )
        .await
        .unwrap();
      assert!(
        !service
          .refresh_rejected(token_fingerprint("local"))
          .await
          .unwrap()
      );
    }

    #[tokio::test]
    async fn already_logged_in_is_retried_once_and_a_second_one_is_shown() {
      let provider = FakeProvider::start().await;
      provider.set(PROVIDER_UP);
      let service = AuthService::new(Some(provider.config()), MemoryStore::default()).unwrap();
      let request = LoginRequest {
        is_login_page_shown: true,
        ..LoginRequest::default()
      };
      let visits = Arc::new(AtomicUsize::new(0));
      let counted = Arc::clone(&visits);
      let outcome = service
        .login(&SignInLook::default(), &request, move |url| {
          tokio::spawn(refuse_every_request(url.to_owned(), counted));
          Ok(())
        })
        .await;
      assert!(
        matches!(&outcome, Err(AuthError::AuthorizationRefused(code)) if code == "already_logged_in"),
        "{outcome:?}"
      );
      assert_eq!(visits.load(Ordering::SeqCst), 2, "one retry, no loop");
      assert_eq!(provider.token_requests(), 0);
    }

    /// A browser at a provider that answers every authorization request with
    /// `already_logged_in`. It follows the app's redirect to the next request, which must keep
    /// `prompt=login` and carry a new `state`.
    async fn refuse_every_request(first_url: String, visits: Arc<AtomicUsize>) {
      let mut maybe_url = Some(first_url);
      let mut states: Vec<String> = Vec::new();
      while let Some(url) = maybe_url {
        visits.fetch_add(1, Ordering::SeqCst);
        let query: std::collections::HashMap<String, String> = url::Url::parse(&url)
          .unwrap()
          .query_pairs()
          .into_owned()
          .collect();
        assert_eq!(query["prompt"], "login");
        assert!(!states.contains(&query["state"]), "a fresh state");
        states.push(query["state"].clone());
        let callback = format!(
          "{}?error=already_logged_in&state={}",
          query["redirect_uri"], query["state"]
        );
        maybe_url = redirect_target(&callback).await;
      }
    }

    /// Sends the browser's GET to the app's loopback; the `Location` of a 303, if it answers one.
    async fn redirect_target(url: &str) -> Option<String> {
      let url = url::Url::parse(url).unwrap();
      let host = format!("{}:{}", url.host_str().unwrap(), url.port().unwrap());
      let mut socket = TcpStream::connect(&host).await.unwrap();
      let request = format!(
        "GET {}?{} HTTP/1.1\r\nHost: {host}\r\n\r\n",
        url.path(),
        url.query().unwrap_or_default()
      );
      socket.write_all(request.as_bytes()).await.unwrap();
      let mut answer = String::new();
      drop(socket.read_to_string(&mut answer).await);
      if !answer.starts_with("HTTP/1.1 303") {
        return None;
      }
      answer
        .lines()
        .find_map(|line| line.strip_prefix("Location: "))
        .map(str::to_owned)
    }

    /// An identity provider on this computer: discovery, and a token endpoint whose answer the test
    /// switches. Each connection serves one request.
    struct FakeProvider {
      base: String,
      mode: Arc<AtomicU8>,
      token_requests: Arc<AtomicUsize>,
      logout_requests: Arc<AtomicUsize>,
    }

    impl FakeProvider {
      async fn start() -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
          .await
          .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mode = Arc::new(AtomicU8::new(PROVIDER_DOWN));
        let token_requests = Arc::new(AtomicUsize::new(0));
        let logout_requests = Arc::new(AtomicUsize::new(0));
        let (serving, answer, counted) = (base.clone(), Arc::clone(&mode), Arc::clone(&token_requests));
        let logouts = Arc::clone(&logout_requests);
        tokio::spawn(async move {
          while let Ok((socket, _)) = listener.accept().await {
            let (base, mode, counted) = (serving.clone(), Arc::clone(&answer), Arc::clone(&counted));
            let logouts = Arc::clone(&logouts);
            tokio::spawn(async move { serve(socket, &base, &mode, &counted, &logouts).await });
          }
        });
        Self {
          base,
          mode,
          token_requests,
          logout_requests,
        }
      }

      fn config(&self) -> DesktopConfig {
        DesktopConfig {
          server_url: "https://team.example.com".into(),
          issuer: self.base.clone(),
          client_id: "teamcraft".into(),
          scopes: vec!["openid".into(), "offline_access".into()],
        }
      }

      fn set(&self, mode: u8) {
        self.mode.store(mode, Ordering::SeqCst);
      }

      fn token_requests(&self) -> usize {
        self.token_requests.load(Ordering::SeqCst)
      }
    }

    fn id_token_of(subject: &str) -> String {
      use base64::engine::general_purpose::URL_SAFE_NO_PAD;
      let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"sub":"{subject}"}}"#));
      format!("eyJhbGciOiJub25lIn0.{payload}.signature")
    }

    async fn serve(
      mut socket: TcpStream,
      base: &str,
      mode: &AtomicU8,
      counted: &AtomicUsize,
      logouts: &AtomicUsize,
    ) {
      let mut request = Vec::new();
      let mut chunk = [0_u8; 4096];
      while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match socket.read(&mut chunk).await {
          Ok(0) | Err(_) => return,
          Ok(read) => request.extend_from_slice(&chunk[..read]),
        }
      }
      let head = String::from_utf8_lossy(&request).into_owned();
      let (status, body) = if head.starts_with("GET /api/v1/me") {
        // The server's answer, at the same address in the tests that ask it.
        if mode.load(Ordering::SeqCst) == PROVIDER_DOWN {
          return;
        }
        ("200 OK", ME_ANSWER.to_owned())
      } else if head.starts_with("POST /logout") {
        logouts.fetch_add(1, Ordering::SeqCst);
        ("200 OK", String::new())
      } else if head.starts_with("GET /.well-known/openid-configuration") {
        let discovery = serde_json::json!({
          "issuer": base,
          "authorization_endpoint": format!("{base}/auth"),
          "token_endpoint": format!("{base}/token"),
          "end_session_endpoint": format!("{base}/logout"),
        });
        if mode.load(Ordering::SeqCst) == PROVIDER_DOWN {
          return;
        }
        ("200 OK", discovery.to_string())
      } else {
        counted.fetch_add(1, Ordering::SeqCst);
        match mode.load(Ordering::SeqCst) {
          // No answer at all, like a network that is not back yet.
          PROVIDER_DOWN => return,
          PROVIDER_UP => (
            "200 OK",
            serde_json::json!({
              "access_token": "fresh-access",
              "token_type": "Bearer",
              "expires_in": 300,
              "refresh_token": "rotated-refresh",
              "id_token": id_token_of(TOKEN_SUBJECT),
            })
            .to_string(),
          ),
          _ => (
            "400 Bad Request",
            serde_json::json!({ "error": "invalid_grant", "error_description": "Token is not active" })
              .to_string(),
          ),
        }
      };
      let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
      );
      drop(socket.write_all(response.as_bytes()).await);
    }

    /// A store whose refresh token can't be deleted.
    #[derive(Default)]
    struct RefusesClear(MemoryStore);

    impl TokenStore for RefusesClear {
      async fn load_note(&self, kind: SessionNote, server: &str) -> Result<Option<String>, AuthError> {
        self.0.load_note(kind, server).await
      }

      async fn save_note(&self, kind: SessionNote, server: &str, note: &str) -> Result<(), AuthError> {
        self.0.save_note(kind, server, note).await
      }

      async fn clear_note(&self, kind: SessionNote, server: &str) -> Result<(), AuthError> {
        self.0.clear_note(kind, server).await
      }

      async fn load(&self, server: &str) -> Result<Option<SecretString>, AuthError> {
        self.0.load(server).await
      }

      async fn save(&self, server: &str, refresh_token: &str) -> Result<(), AuthError> {
        self.0.save(server, refresh_token).await
      }

      async fn clear(&self, _server: &str) -> Result<(), AuthError> {
        Err(AuthError::Unsupported("the disk is full".to_owned()))
      }

      async fn load_local_session(&self, server: &str) -> Result<Option<SecretString>, AuthError> {
        self.0.load_local_session(server).await
      }

      async fn save_local_session(&self, server: &str, session: &SecretString) -> Result<(), AuthError> {
        self.0.save_local_session(server, session).await
      }

      async fn clear_local_session(&self, server: &str) -> Result<(), AuthError> {
        self.0.clear_local_session(server).await
      }
    }

    /// A keychain in memory that counts every access.
    #[derive(Default)]
    struct CountingKeystore {
      calls: AtomicUsize,
      entries: std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>,
    }

    impl Keystore for CountingKeystore {
      fn read_secret(
        &self,
        service: &str,
        account: &str,
      ) -> Result<Option<Zeroizing<Vec<u8>>>, KeySourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let entries = self.entries.lock().unwrap();
        Ok(
          entries
            .get(&format!("{service}/{account}"))
            .cloned()
            .map(Zeroizing::new),
        )
      }

      fn read_text(&self, service: &str, account: &str) -> Result<Option<Zeroizing<String>>, KeySourceError> {
        Ok(
          self
            .read_secret(service, account)?
            .map(|bytes| Zeroizing::new(String::from_utf8(bytes.to_vec()).unwrap())),
        )
      }

      fn write_secret(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), KeySourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut entries = self.entries.lock().unwrap();
        entries.insert(format!("{service}/{account}"), secret.to_vec());
        Ok(())
      }

      fn delete(&self, service: &str, account: &str) -> Result<(), KeySourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self
          .entries
          .lock()
          .unwrap()
          .remove(&format!("{service}/{account}"));
        Ok(())
      }
    }

    /// A service signed in at [`START_MS`] with a token for [`LIFETIME_MS`], and its clock.
    fn signed_in(server: DesktopConfig) -> (AuthService<MemoryStore>, Arc<AtomicI64>) {
      let clock = Arc::new(AtomicI64::new(START_MS));
      let reading = Arc::clone(&clock);
      let service = AuthService::with_clock(
        Some(server),
        MemoryStore::default(),
        Arc::new(move || reading.load(Ordering::SeqCst)),
      );
      let tokens = Tokens {
        access: SecretString::from("access".to_owned()),
        refresh: Some(SecretString::from("refresh".to_owned())),
        expires_at_ms: START_MS + LIFETIME_MS,
        refresh_at_ms: START_MS + LIFETIME_MS - 60_000,
        is_from_refusal: false,
      };
      service
        .session
        .try_lock()
        .expect("nobody else holds the new session")
        .maybe_tokens = Some(tokens);
      (service, clock)
    }

    fn config() -> DesktopConfig {
      DesktopConfig {
        server_url: "https://team.example.com".into(),
        issuer: "https://auth.example.com/realms/company".into(),
        client_id: "teamcraft".into(),
        scopes: vec!["openid".into()],
      }
    }
  }

  /// DPoP on the desktop (e2ee.md §3.1.2) against a fake server and identity provider at one address.
  mod dpop {
    use std::sync::Mutex as TestMutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use teamcraft_core::store::{KeySourceError, Keystore, SecretVault};
    use teamcraft_crypto::dpop::DpopPublicKey;
    use zeroize::Zeroizing;

    use super::*;
    use crate::auth::store::MemoryStore;

    const NONCE: &str = "server-nonce";
    const PROVIDER_NONCE: &str = "provider-nonce";

    /// What the fake does.
    #[derive(Default)]
    struct Script {
      /// The discovery document lists ES256.
      advertises: AtomicBool,
      /// `server-info` offers a nonce: a server that checks proofs.
      offers_nonce: AtomicBool,
      /// The token endpoint asks for its own nonce first.
      wants_nonce: AtomicBool,
      /// The proofs the token endpoint saw, `None` for a request without one.
      token_proofs: TestMutex<Vec<Option<String>>>,
      /// What the end-session endpoint saw.
      logout_proofs: TestMutex<Vec<Option<String>>>,
      /// How many requests reached `/api/v1/me`.
      me_requests: AtomicUsize,
    }

    struct Fake {
      base: String,
      script: Arc<Script>,
    }

    impl Fake {
      async fn start() -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
          .await
          .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let script = Arc::new(Script::default());
        let (serving, shared) = (base.clone(), Arc::clone(&script));
        tokio::spawn(async move {
          while let Ok((socket, _)) = listener.accept().await {
            let (base, script) = (serving.clone(), Arc::clone(&shared));
            tokio::spawn(async move { serve(socket, &base, &script).await });
          }
        });
        Self { base, script }
      }

      /// A provider and a server that both do DPoP.
      async fn with_dpop() -> Self {
        let fake = Self::start().await;
        fake.script.advertises.store(true, Ordering::SeqCst);
        fake.script.offers_nonce.store(true, Ordering::SeqCst);
        fake
      }

      fn config(&self) -> DesktopConfig {
        DesktopConfig {
          server_url: self.base.clone(),
          issuer: self.base.clone(),
          client_id: "teamcraft".into(),
          scopes: vec!["openid".into(), "offline_access".into()],
        }
      }

      fn token_proofs(&self) -> Vec<Option<String>> {
        self.script.token_proofs.lock().unwrap().clone()
      }
    }

    async fn serve(mut socket: tokio::net::TcpStream, base: &str, script: &Script) {
      let mut request = Vec::new();
      let mut chunk = [0_u8; 8192];
      while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match socket.read(&mut chunk).await {
          Ok(0) | Err(_) => return,
          Ok(read) => request.extend_from_slice(&chunk[..read]),
        }
      }
      let head = String::from_utf8_lossy(&request).into_owned();
      let proof = header_of(&head, "dpop");
      let (status, extra, body) = if head.starts_with("GET /.well-known/openid-configuration") {
        let mut discovery = serde_json::json!({
          "issuer": base,
          "authorization_endpoint": format!("{base}/auth"),
          "token_endpoint": format!("{base}/token"),
          "end_session_endpoint": format!("{base}/logout"),
        });
        if script.advertises.load(Ordering::SeqCst) {
          discovery["dpop_signing_alg_values_supported"] = serde_json::json!(["ES256"]);
        }
        ("200 OK", String::new(), discovery.to_string())
      } else if head.starts_with("GET /api/v1/server-info") {
        let extra = if script.offers_nonce.load(Ordering::SeqCst) {
          format!("DPoP-Nonce: {NONCE}\r\n")
        } else {
          String::new()
        };
        ("200 OK", extra, "{}".to_owned())
      } else if head.starts_with("GET /api/v1/me") {
        script.me_requests.fetch_add(1, Ordering::SeqCst);
        answer_me(&head, proof.as_deref())
      } else if head.starts_with("POST /logout") {
        script.logout_proofs.lock().unwrap().push(proof);
        ("200 OK", String::new(), String::new())
      } else {
        answer_token(script, proof)
      };
      let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
      );
      drop(socket.write_all(response.as_bytes()).await);
    }

    fn answer_token(script: &Script, proof: Option<String>) -> (&'static str, String, String) {
      script.token_proofs.lock().unwrap().push(proof.clone());
      let Some(proof) = proof else {
        return (
          "200 OK",
          String::new(),
          token_answer("Bearer", &jwt(r#"{"sub":"alice"}"#)),
        );
      };
      let claims = claims_of(&proof);
      if script.wants_nonce.load(Ordering::SeqCst) && claims["nonce"] != PROVIDER_NONCE {
        return (
          "400 Bad Request",
          format!("DPoP-Nonce: {PROVIDER_NONCE}\r\n"),
          r#"{"error":"use_dpop_nonce"}"#.to_owned(),
        );
      }
      let jkt = thumbprint_of(&proof);
      let access = jwt(&format!(
        r#"{{"sub":"alice","typ":"DPoP","cnf":{{"jkt":"{}"}}}}"#,
        jkt.as_str()
      ));
      ("200 OK", String::new(), token_answer("DPoP", &access))
    }

    /// The server: a bound token needs the `DPoP` scheme and a proof with its nonce.
    fn answer_me(head: &str, proof: Option<&str>) -> (&'static str, String, String) {
      let authorization = header_of(head, "authorization").unwrap_or_default();
      if authorization.starts_with("Bearer ") {
        return (
          "200 OK",
          String::new(),
          r#"{"id":"42","display_name":"Alice","role":"owner"}"#.to_owned(),
        );
      }
      let nonce = proof.map(|proof| claims_of(proof)["nonce"].clone());
      if nonce.as_ref().and_then(|nonce| nonce.as_str()) != Some(NONCE) {
        return (
          "401 Unauthorized",
          format!("DPoP-Nonce: {NONCE}\r\n"),
          r#"{"error_code":"use_dpop_nonce","message":"the nonce"}"#.to_owned(),
        );
      }
      (
        "200 OK",
        format!("DPoP-Nonce: {NONCE}\r\n"),
        r#"{"id":"42","display_name":"Alice","role":"owner"}"#.to_owned(),
      )
    }

    fn token_answer(token_type: &str, access: &str) -> String {
      serde_json::json!({
        "access_token": access,
        "token_type": token_type,
        "expires_in": 300,
        "refresh_token": "rotated-refresh",
      })
      .to_string()
    }

    fn header_of(head: &str, name: &str) -> Option<String> {
      head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim().to_owned())
      })
    }

    fn jwt(claims: &str) -> String {
      format!("e30.{}.c2ln", URL_SAFE_NO_PAD.encode(claims))
    }

    fn claims_of(proof: &str) -> serde_json::Value {
      let part = proof.split('.').nth(1).unwrap();
      serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
    }

    fn thumbprint_of(proof: &str) -> JwkThumbprint {
      let part = proof.split('.').next().unwrap();
      let header: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap();
      let jwk = &header["jwk"];
      DpopPublicKey::from_coordinates(jwk["x"].as_str().unwrap(), jwk["y"].as_str().unwrap())
        .unwrap()
        .thumbprint()
    }

    /// A store with the refresh token of an earlier sign-in, and the service on it.
    async fn starting(fake: &Fake, store: MemoryStore) -> AuthService<MemoryStore> {
      store
        .save(&fake.config().token_key(), "stored-refresh")
        .await
        .unwrap();
      AuthService::new(Some(fake.config()), store).unwrap()
    }

    /// The same store, as a restart finds it.
    fn after_restart(service: AuthService<MemoryStore>, fake: &Fake) -> AuthService<MemoryStore> {
      AuthService::new(Some(fake.config()), service.store).unwrap()
    }

    mod binding {
      use super::*;

      #[tokio::test]
      async fn a_provider_and_a_server_that_do_dpop_bind_the_session() {
        let fake = Fake::with_dpop().await;
        let service = starting(&fake, MemoryStore::default()).await;
        assert!(service.restore().await.unwrap());
        let proofs = fake.token_proofs();
        assert_eq!(proofs.len(), 1);
        let proof = proofs[0].as_deref().expect("a proof at the token endpoint");
        assert_eq!(claims_of(proof)["htm"], "POST");
        assert_eq!(claims_of(proof)["htu"], format!("{}/token", fake.base));
        assert!(claims_of(proof).get("ath").is_none());
        let token = service.bearer_token().await.unwrap();
        assert_eq!(
          token_bound_key(&token),
          service.peek_dpop_prover().map(|prover| prover.thumbprint())
        );
      }

      #[tokio::test]
      async fn a_provider_without_dpop_gets_no_proof_and_no_key_is_made() {
        let fake = Fake::start().await;
        fake.script.offers_nonce.store(true, Ordering::SeqCst);
        let service = starting(&fake, MemoryStore::default()).await;
        assert!(service.restore().await.unwrap());
        assert_eq!(fake.token_proofs(), vec![None]);
        assert!(service.peek_dpop_prover().is_none());
        assert_eq!(
          service
            .store
            .load_note(SessionNote::DpopKey, &fake.config().server_url)
            .await
            .unwrap(),
          None
        );
      }

      /// An older server, or a proxy that strips `DPoP-Nonce`: it would refuse a bound token.
      #[tokio::test]
      async fn a_server_that_offers_no_nonce_keeps_the_session_unbound() {
        let fake = Fake::start().await;
        fake.script.advertises.store(true, Ordering::SeqCst);
        let service = starting(&fake, MemoryStore::default()).await;
        assert!(service.restore().await.unwrap());
        assert_eq!(fake.token_proofs(), vec![None]);
        let answer = service.api_get(ME_PATH).await.unwrap();
        assert_eq!(answer.status, 200);
      }

      #[tokio::test]
      async fn the_providers_nonce_challenge_gets_one_more_try() {
        let fake = Fake::with_dpop().await;
        fake.script.wants_nonce.store(true, Ordering::SeqCst);
        let service = starting(&fake, MemoryStore::default()).await;
        assert!(service.restore().await.unwrap());
        let proofs = fake.token_proofs();
        assert_eq!(proofs.len(), 2);
        assert_eq!(claims_of(proofs[1].as_deref().unwrap())["nonce"], PROVIDER_NONCE);
      }
    }

    mod requests {
      use super::*;

      #[tokio::test]
      async fn an_api_request_meets_the_challenge_once_and_then_has_the_nonce() {
        let fake = Fake::start().await;
        fake.script.advertises.store(true, Ordering::SeqCst);
        fake.script.offers_nonce.store(true, Ordering::SeqCst);
        let service = starting(&fake, MemoryStore::default()).await;
        service.restore().await.unwrap();
        // The nonce of `server-info` came along: no challenge at all.
        assert_eq!(service.api_get(ME_PATH).await.unwrap().status, 200);
        assert_eq!(fake.script.me_requests.load(Ordering::SeqCst), 1);

        // An old nonce in memory (a server restart, a long idle time): one retry.
        service.peek_dpop_prover().unwrap().remember_nonce("old");
        assert_eq!(service.api_get(ME_PATH).await.unwrap().status, 200);
        assert_eq!(fake.script.me_requests.load(Ordering::SeqCst), 3);
      }
    }

    mod restart {
      use super::*;

      #[tokio::test]
      async fn a_restart_keeps_the_key_and_the_refresh_token_works() {
        let fake = Fake::with_dpop().await;
        let service = starting(&fake, MemoryStore::default()).await;
        service.restore().await.unwrap();
        let bound = service.peek_dpop_prover().unwrap().thumbprint();

        let restarted = after_restart(service, &fake);
        assert!(restarted.restore().await.unwrap());
        let proofs = fake.token_proofs();
        assert_eq!(proofs.len(), 2);
        assert_eq!(thumbprint_of(proofs[1].as_deref().unwrap()), bound);
        assert_eq!(restarted.peek_dpop_prover().unwrap().thumbprint(), bound);
        // The nonce came with the refresh: the first request after the start has no challenge.
        assert_eq!(restarted.api_get(ME_PATH).await.unwrap().status, 200);
        assert_eq!(fake.script.me_requests.load(Ordering::SeqCst), 1);
      }

      /// The provider stopped advertising, or the server is older now: a bound refresh token still
      /// takes its proof, or it is refused.
      #[tokio::test]
      async fn a_bound_refresh_token_keeps_its_proof_whatever_the_discovery_says() {
        let fake = Fake::with_dpop().await;
        let service = starting(&fake, MemoryStore::default()).await;
        service.restore().await.unwrap();
        fake.script.advertises.store(false, Ordering::SeqCst);
        fake.script.offers_nonce.store(false, Ordering::SeqCst);

        let restarted = after_restart(service, &fake);
        assert!(restarted.restore().await.unwrap());
        assert!(fake.token_proofs()[1].is_some());
        // Without a nonce from `server-info`, the first request meets the challenge and passes it.
        assert_eq!(restarted.api_get(ME_PATH).await.unwrap().status, 200);
        assert_eq!(fake.script.me_requests.load(Ordering::SeqCst), 2);
      }

      #[tokio::test]
      async fn a_lost_key_ends_the_session_and_says_so() {
        let fake = Fake::with_dpop().await;
        let service = starting(&fake, MemoryStore::default()).await;
        service.restore().await.unwrap();
        let config = fake.config();
        service
          .store
          .clear_note(SessionNote::DpopKey, &config.server_url)
          .await
          .unwrap();

        let restarted = after_restart(service, &fake);
        let error = restarted.restore().await.unwrap_err();
        assert!(matches!(error, AuthError::KeyLost), "{error}");
        assert_eq!(error.kind(), "key_lost");
        assert_eq!(fake.token_proofs().len(), 1, "nothing goes to the provider");
        assert!(matches!(restarted.peek_server_bearer_token(), TokenPeek::KeyLost));
        assert!(
          restarted.store.load(&config.token_key()).await.unwrap().is_none(),
          "the refresh token that needs it goes too"
        );
      }

      #[tokio::test]
      async fn a_sign_out_ends_the_bound_session_with_a_proof_and_keeps_the_key() {
        let fake = Fake::with_dpop().await;
        let service = starting(&fake, MemoryStore::default()).await;
        service.restore().await.unwrap();
        service.logout().await.unwrap();
        let proofs = fake.script.logout_proofs.lock().unwrap().clone();
        assert_eq!(proofs.len(), 1);
        assert!(proofs[0].is_some());
        let config = fake.config();
        assert_eq!(
          service
            .store
            .load_note(SessionNote::TokenBinding, &config.token_key())
            .await
            .unwrap(),
          None
        );
        assert!(
          service
            .store
            .load_note(SessionNote::DpopKey, &config.server_url)
            .await
            .unwrap()
            .is_some()
        );
      }
    }

    /// #1150: the key is in the sealed file, so it costs no keychain access at the start or later.
    mod keychain {
      use super::*;

      #[derive(Default)]
      struct CountingKeystore {
        calls: AtomicUsize,
        secrets: TestMutex<std::collections::HashMap<String, Vec<u8>>>,
      }

      impl Keystore for CountingKeystore {
        fn read_secret(
          &self,
          service: &str,
          account: &str,
        ) -> Result<Option<Zeroizing<Vec<u8>>>, KeySourceError> {
          self.calls.fetch_add(1, Ordering::SeqCst);
          let secrets = self.secrets.lock().unwrap();
          Ok(
            secrets
              .get(&format!("{service}/{account}"))
              .cloned()
              .map(Zeroizing::new),
          )
        }

        fn read_text(
          &self,
          _service: &str,
          _account: &str,
        ) -> Result<Option<Zeroizing<String>>, KeySourceError> {
          self.calls.fetch_add(1, Ordering::SeqCst);
          Ok(None)
        }

        fn write_secret(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), KeySourceError> {
          self.calls.fetch_add(1, Ordering::SeqCst);
          let mut secrets = self.secrets.lock().unwrap();
          secrets.insert(format!("{service}/{account}"), secret.to_vec());
          Ok(())
        }

        fn delete(&self, _service: &str, _account: &str) -> Result<(), KeySourceError> {
          self.calls.fetch_add(1, Ordering::SeqCst);
          Ok(())
        }
      }

      #[tokio::test(flavor = "multi_thread")]
      async fn making_and_using_the_key_costs_no_keychain_access() {
        let fake = Fake::with_dpop().await;
        let keystore = Arc::new(CountingKeystore::default());
        let directory = std::env::temp_dir().join(format!("teamcraft-dpop-vault-{}", std::process::id()));
        let open_vault = || {
          let vault = Arc::new(SecretVault::new(Arc::clone(&keystore) as Arc<dyn Keystore>));
          vault.set_directory(&directory);
          vault
        };
        let first = VaultStore::new(open_vault());
        first.save(&fake.config().token_key(), "refresh").await.unwrap();
        first
          .clear_local_session(&fake.config().server_url)
          .await
          .unwrap();
        drop(first);
        keystore.calls.store(0, Ordering::SeqCst);

        // The first start with DPoP: the key is made.
        let service = AuthService::new(Some(fake.config()), VaultStore::new(open_vault())).unwrap();
        assert!(service.restore().await.unwrap());
        assert_eq!(keystore.calls.load(Ordering::SeqCst), 1, "one read per start");
        let bound = service.peek_dpop_prover().unwrap().thumbprint();
        keystore.calls.store(0, Ordering::SeqCst);
        for _ in 0..3 {
          assert_eq!(service.api_get(ME_PATH).await.unwrap().status, 200);
        }
        assert_eq!(
          keystore.calls.load(Ordering::SeqCst),
          0,
          "no access while running"
        );
        drop(service);

        // The next start: one read for the file, and the key is in it.
        let restarted = AuthService::new(Some(fake.config()), VaultStore::new(open_vault())).unwrap();
        assert!(restarted.restore().await.unwrap());
        assert_eq!(keystore.calls.load(Ordering::SeqCst), 1, "one read per start");
        assert_eq!(restarted.peek_dpop_prover().unwrap().thumbprint(), bound);
        drop(std::fs::remove_dir_all(&directory));
      }
    }
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
