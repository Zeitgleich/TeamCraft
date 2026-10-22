//! Sign-in: the contract between the UI and whatever performs OIDC login and authenticated API calls.
//!
//! [`Authenticator`] is deliberately small so `teamcraft-core` can own the implementation later. Two
//! implementations exist today: [`browser::BrowserAuth`] (authorization code + PKCE redirect flow in
//! the page) and [`tauri::TauriAuth`] (the desktop shell does the flow on the Rust side with the
//! system browser and keeps tokens in the OS keyring, so the webview never sees them).

pub mod browser;
mod config;
pub(crate) mod fetch;
pub mod tauri;

use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

pub use config::AppConfig;
pub use teamcraft_client_api::remote::ApiMethod;

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
}

/// Response of an authenticated API call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiResponse {
  pub status: u16,
  pub body: String,
}

/// Performs login and authenticated API calls.
///
/// API calls live on this trait, not on a token getter, so an implementation can keep the tokens out
/// of reach of the UI (desktop) and refresh them transparently.
pub trait Authenticator {
  /// Completes a redirect that is in progress or loads a stored session. `Ok(true)` when signed in.
  fn restore(&self) -> LocalFuture<'_, Result<bool, AuthError>>;
  /// Starts a login. In the browser this navigates away and never resolves; on desktop it resolves when
  /// the system browser has returned.
  fn login(&self) -> LocalFuture<'_, Result<(), AuthError>>;
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

  /// `GET {server}{path}`, see [`Self::api_request`].
  fn api_get(&self, path: &str) -> LocalFuture<'_, Result<ApiResponse, AuthError>> {
    self.api_request(ApiMethod::Get, path, None)
  }
}

/// Picks the implementation for the current platform.
pub fn detect(config: AppConfig) -> Rc<dyn Authenticator> {
  if tauri::is_available() {
    Rc::new(tauri::TauriAuth::new())
  } else {
    let server_url = config.server_url.clone();
    let auth: Rc<dyn Authenticator> = Rc::new(browser::BrowserAuth::new(config));
    // The page runs the client core itself (web_core), signing with the page's session.
    #[cfg(target_arch = "wasm32")]
    crate::web_core::configure(auth.clone(), &server_url);
    #[cfg(not(target_arch = "wasm32"))]
    drop(server_url);
    auth
  }
}
