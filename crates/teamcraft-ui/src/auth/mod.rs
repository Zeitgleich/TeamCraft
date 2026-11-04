//! Sign-in: the contract between the UI and whatever performs OIDC login and authenticated API calls.
//!
//! [`Authenticator`] is deliberately small so `teamcraft-core` can own the implementation later. Two
//! implementations exist today: [`browser::BrowserAuth`] (authorization code + PKCE redirect flow in
//! the page) and [`tauri::TauriAuth`] (the desktop shell does the flow on the Rust side with the
//! system browser and keeps tokens in the OS keyring, so the webview never sees them).

mod account_gate;
pub mod browser;
mod config;
pub(crate) mod fetch;
pub mod guest;
pub mod tauri;
mod token_vault;

use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use account_gate::AccountGate;
pub use account_gate::ME_PATH;
pub use config::AppConfig;
pub use teamcraft_client_api::remote::ApiMethod;
pub use teamcraft_client_api::sign_in_look::SignInLook;
use teamcraft_core::api::dpop::DpopProver;
use teamcraft_proto::GetMeResponse;

/// A boxed future that does not need to be `Send`; everything here runs on the UI thread.
pub type LocalFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
  /// The user closed the login or the sign-in timed out.
  #[error("sign-in was cancelled")]
  Cancelled,
  /// The identity provider refused the request (`error` and `error_description` of RFC 6749).
  #[error("the identity provider refused the request: {0}")]
  Denied(String),
  /// The identity provider answered the sign-in redirect with an `error` (RFC 6749 §4.1.2.1); the
  /// code as [`shown_error_code`](teamcraft_client_api::oidc_login::shown_error_code) cleaned it.
  #[error("the identity provider refused the sign-in: {0}")]
  AuthorizationRefused(String),
  /// The identity provider or the server could not be reached.
  #[error("network error: {0}")]
  Network(String),
  /// A response did not have the expected shape.
  #[error("unexpected response: {0}")]
  InvalidResponse(String),
  /// The `state` of the redirect does not match the one that was sent (possible CSRF).
  #[error("the sign-in response did not match the request")]
  StateMismatch,
  /// There is no valid session; the user has to sign in again.
  #[error("not signed in")]
  NotSignedIn,
  /// The platform lacks something the flow needs (e.g. no keyring).
  #[error("not supported here: {0}")]
  Unsupported(String),
  /// The sign-in is bound to a key this device doesn't have any more (e2ee.md §3.1.2): the user has
  /// to sign in again, and is told why.
  #[error("the key this sign-in is bound to is gone")]
  KeyLost,
}

/// Response of an authenticated API call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiResponse {
  pub status: u16,
  pub body: String,
}

/// A binary request body with its media type (brand images).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinaryBody {
  pub content_type: String,
  pub bytes: Vec<u8>,
}

/// Response of an authenticated API call whose body is not text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiBytes {
  pub status: u16,
  pub maybe_content_type: Option<String>,
  pub body: Vec<u8>,
}

/// Performs login and authenticated API calls.
///
/// API calls live on this trait, not on a token getter, so an implementation can keep the tokens out
/// of reach of the UI (desktop) and refresh them transparently.
pub trait Authenticator {
  /// Completes a redirect that is in progress or loads a stored session. `Ok(true)` when signed in.
  fn restore(&self) -> LocalFuture<'_, Result<bool, AuthError>>;
  /// The account the server confirmed last on this device, if its session is still stored, read
  /// without the network: the app starts from it at once while [`Self::restore`] and `GET /me` run
  /// beside it (issue #778). It only shows local data; nothing goes out until the server confirmed
  /// it ([`account_gate`]). Not during a redirect back from the identity provider, which must finish
  /// first.
  fn cached_account(&self) -> LocalFuture<'_, Option<GetMeResponse>> {
    Box::pin(async { None })
  }
  /// The server confirmed `account`: requests may go out, and the next start can begin with it.
  fn remember_account<'a>(&'a self, _account: &'a GetMeResponse) -> LocalFuture<'a, ()> {
    Box::pin(async {})
  }
  /// The cached account doesn't stand: the next start waits for the server again.
  fn forget_account(&self) -> LocalFuture<'_, ()> {
    Box::pin(async {})
  }
  /// The app started from the cached account and the server hasn't confirmed it yet.
  fn is_account_unconfirmed(&self) -> bool {
    false
  }
  /// Starts a login. In the browser this navigates away and never resolves; on desktop it resolves when
  /// the system browser has returned.
  fn login(&self, look: SignInLook) -> LocalFuture<'_, Result<(), AuthError>>;
  /// Aborts a login that waits for the system browser. Nothing to do in the browser flow.
  fn cancel_login(&self) {}
  /// Ends the session and forgets stored tokens.
  fn logout(&self) -> LocalFuture<'_, Result<(), AuthError>>;
  /// `{method} {server}{path}` with the current access token, refreshed first when it is about to
  /// expire. A body is sent as JSON.
  fn api_request(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<String>,
  ) -> LocalFuture<'_, Result<ApiResponse, AuthError>>;

  /// Signs in with a local account (people without an identity provider account): e-mail, password
  /// and, when the account has one, the current code of the authenticator app. A refusal comes back
  /// as `Denied(code)` with the server's error code (browser) or the shell's kind (desktop).
  fn login_local(
    &self,
    _login: String,
    _password: String,
    _maybe_totp: Option<String>,
  ) -> LocalFuture<'_, Result<(), AuthError>> {
    Box::pin(async {
      Err(AuthError::Unsupported(
        "local accounts are not available here".to_owned(),
      ))
    })
  }

  /// A valid access token, refreshed when it is about to expire. Only the browser build has one to
  /// give: the client core runs in the page and signs its own requests. The desktop shell keeps
  /// tokens out of the webview.
  fn access_token(&self) -> LocalFuture<'_, Result<String, AuthError>> {
    Box::pin(async {
      Err(AuthError::Unsupported(
        "the tokens stay in the desktop shell".to_owned(),
      ))
    })
  }

  /// The newest ID token of the session, for role signing in the page's core (e2ee.md §3.1.3). `None`
  /// until the provider sent one since the page loaded. Not a credential at the server.
  fn id_token(&self) -> Option<String> {
    None
  }

  /// Gets a new ID token now rather than at the next refresh: what it should name was just written.
  fn refresh_id_token(&self) -> LocalFuture<'_, ()> {
    Box::pin(async {})
  }

  /// What signs DPoP proofs for the page's core, next to [`Self::access_token`]. The desktop shell
  /// signs itself.
  fn dpop_prover(&self) -> LocalFuture<'_, Option<Arc<DpopProver>>> {
    Box::pin(async { None })
  }

  /// Like [`Self::api_request`] for bodies that are bytes: uploads of images and downloads of them.
  fn api_request_bytes(
    &self,
    _method: ApiMethod,
    _path: &str,
    _maybe_body: Option<BinaryBody>,
  ) -> LocalFuture<'_, Result<ApiBytes, AuthError>> {
    Box::pin(async {
      Err(AuthError::Unsupported(
        "binary requests are not available here".to_owned(),
      ))
    })
  }

  /// `GET {server}{path}`, see [`Self::api_request`].
  fn api_get(&self, path: &str) -> LocalFuture<'_, Result<ApiResponse, AuthError>> {
    self.api_request(ApiMethod::Get, path, None)
  }
}

/// Picks the implementation for the current platform.
pub fn detect(config: AppConfig) -> Rc<dyn Authenticator> {
  if tauri::is_available() {
    Rc::new(AccountGate::new(Rc::new(tauri::TauriAuth::new())))
  } else {
    let server_url = config.server_url.clone();
    let auth: Rc<dyn Authenticator> = Rc::new(AccountGate::new(Rc::new(browser::BrowserAuth::new(config))));
    // The page runs the client core itself (web_core), signing with the page's session.
    #[cfg(target_arch = "wasm32")]
    crate::web_core::configure(auth.clone(), &server_url);
    #[cfg(not(target_arch = "wasm32"))]
    drop(server_url);
    auth
  }
}
