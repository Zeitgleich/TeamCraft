//! OIDC authorization code flow with PKCE in the browser (redirect flow).
//!
//! The page navigates to the identity provider and comes back to `<origin>/` with `?code=…&state=…`.
//! Only the PKCE verifier and the `state` survive the redirect, in `sessionStorage`; tokens live in
//! memory only, so a reload signs in again through the provider's session (a redirect round trip
//! without a password prompt) until the server offers a cookie-based refresh.

use std::cell::RefCell;

use oauth2::basic::BasicClient;
use oauth2::{
  AuthUrl, AuthorizationCode, ClientId, CsrfToken, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl,
  RefreshToken, RequestTokenError, Scope, TokenResponse, TokenUrl,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use teamcraft_client_api::server_entry::{HttpPolicy, SERVER_INFO_PATH, check_discovery};
use teamcraft_proto::{SERVER_INFO_PRODUCT, ServerInfoResponse};

use super::fetch::fetch;
use super::{ApiBytes, ApiMethod, ApiResponse, AppConfig, AuthError, Authenticator, BinaryBody, LocalFuture};
use crate::clock::now_ms;

const PENDING_KEY: &str = "teamcraft.oidc.pending";
const SIGNED_IN_HINT_KEY: &str = "teamcraft.signed_in";
const AUTOLOGIN_TRIED_KEY: &str = "teamcraft.autologin_tried";
/// The bearer token of a local account, for the life of the tab.
const LOCAL_SESSION_KEY: &str = "teamcraft.local_session";
/// The scope every sign-in asks for, also with development overrides.
const OPENID_SCOPE: &str = "openid";
/// Refresh this long before the access token expires.
const REFRESH_MARGIN_MS: i64 = 30_000;

pub struct BrowserAuth {
  config: AppConfig,
  /// Set while a local account (guest) is signed in instead of an identity provider account.
  local: RefCell<Option<LocalSession>>,
  tokens: RefCell<Option<Tokens>>,
  sign_in: RefCell<Option<SignIn>>,
  discovery: RefCell<Option<Discovery>>,
}

/// How to sign in: the server's settings, or the development overrides of [`AppConfig`].
#[derive(Clone, Debug)]
struct SignIn {
  issuer: String,
  client_id: String,
  scopes: Vec<String>,
}

/// The session of a local account: the server issues the token, there is no refresh.
#[derive(Clone, Serialize, Deserialize)]
struct LocalSession {
  token: String,
  expires_at_ms: i64,
}

struct Tokens {
  access: SecretString,
  maybe_refresh: Option<SecretString>,
  expires_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize)]
struct Discovery {
  issuer: String,
  authorization_endpoint: String,
  token_endpoint: String,
  #[serde(default, rename = "end_session_endpoint")]
  maybe_end_session_endpoint: Option<String>,
}

/// What must survive the redirect to the identity provider.
#[derive(Serialize, Deserialize)]
struct Pending {
  state: String,
  verifier: String,
  redirect_uri: String,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct HttpError(String);

impl BrowserAuth {
  pub fn new(config: AppConfig) -> Self {
    Self {
      config,
      local: RefCell::new(None),
      tokens: RefCell::new(None),
      sign_in: RefCell::new(None),
      discovery: RefCell::new(None),
    }
  }

  async fn restore_inner(&self) -> Result<bool, AuthError> {
    if let Some(stored) = session_get(LOCAL_SESSION_KEY)
      && let Ok(session) = serde_json::from_str::<LocalSession>(&stored)
      && session.expires_at_ms > now_ms()
    {
      *self.local.borrow_mut() = Some(session);
      return Ok(true);
    }
    session_remove(LOCAL_SESSION_KEY);
    let params = location_params();
    if let Some(error) = params.get("error") {
      let description = params.get("error_description").unwrap_or(error);
      clean_location();
      session_remove(PENDING_KEY);
      return Err(AuthError::Denied(description.clone()));
    }
    if let (Some(code), Some(state)) = (params.get("code"), params.get("state")) {
      let outcome = self.finish_login(code, state).await;
      clean_location();
      return outcome.map(|()| true);
    }
    // Signed in before and the tab was reloaded: go through the provider again, once, without asking.
    if local_get(SIGNED_IN_HINT_KEY).is_some() && session_get(AUTOLOGIN_TRIED_KEY).is_none() {
      session_set(AUTOLOGIN_TRIED_KEY, "1");
      self.begin_login().await?;
      std::future::pending::<()>().await;
    }
    Ok(false)
  }

  async fn begin_login(&self) -> Result<(), AuthError> {
    let sign_in = self.sign_in().await?;
    let discovery = self.discovery().await?;
    let redirect_uri = redirect_uri()?;
    let client = self.client(&discovery, &redirect_uri, &sign_in)?;
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, state) = client
      .authorize_url(CsrfToken::new_random)
      .add_scopes(sign_in.scopes.iter().cloned().map(Scope::new))
      .set_pkce_challenge(challenge)
      .url();
    let pending = Pending {
      state: state.secret().clone(),
      verifier: verifier.secret().clone(),
      redirect_uri,
    };
    let pending =
      serde_json::to_string(&pending).map_err(|error| AuthError::InvalidResponse(error.to_string()))?;
    session_set(PENDING_KEY, &pending);
    navigate(url.as_str())
  }

  async fn finish_login(&self, code: &str, state: &str) -> Result<(), AuthError> {
    let pending = session_get(PENDING_KEY).ok_or(AuthError::StateMismatch)?;
    session_remove(PENDING_KEY);
    let pending: Pending = serde_json::from_str(&pending).map_err(|_| AuthError::StateMismatch)?;
    if pending.state != state {
      return Err(AuthError::StateMismatch);
    }
    let sign_in = self.sign_in().await?;
    let discovery = self.discovery().await?;
    let client = self.client(&discovery, &pending.redirect_uri, &sign_in)?;
    let response = client
      .exchange_code(AuthorizationCode::new(code.to_owned()))
      .set_pkce_verifier(PkceCodeVerifier::new(pending.verifier))
      .request_async(&http_client)
      .await
      .map_err(map_token_error)?;
    self.store(&response);
    local_set(SIGNED_IN_HINT_KEY, "1");
    session_remove(AUTOLOGIN_TRIED_KEY);
    Ok(())
  }

  async fn refresh(&self) -> Result<(), AuthError> {
    let refresh = self
      .tokens
      .borrow()
      .as_ref()
      .and_then(|tokens| {
        tokens
          .maybe_refresh
          .as_ref()
          .map(|token| token.expose_secret().to_owned())
      })
      .ok_or(AuthError::NotSignedIn)?;
    let sign_in = self.sign_in().await?;
    let discovery = self.discovery().await?;
    let client = self.client(&discovery, &redirect_uri()?, &sign_in)?;
    match client
      .exchange_refresh_token(&RefreshToken::new(refresh))
      .request_async(&http_client)
      .await
    {
      Ok(response) => {
        self.store(&response);
        Ok(())
      },
      Err(error) => {
        // An expired or revoked refresh token ends the session; other errors may be transient.
        if matches!(error, RequestTokenError::ServerResponse(_)) {
          self.tokens.replace(None);
          return Err(AuthError::NotSignedIn);
        }
        Err(map_token_error(error))
      },
    }
  }

  async fn login_local_inner(
    &self,
    login: String,
    password: String,
    maybe_totp: Option<String>,
  ) -> Result<(), AuthError> {
    let request = serde_json::json!({
      "login": login,
      "password": password,
      "totp_code": maybe_totp.filter(|code| !code.trim().is_empty()),
    });
    let headers = [
      ("Content-Type".to_owned(), "application/json".to_owned()),
      ("Accept".to_owned(), "application/json".to_owned()),
    ];
    let response = fetch(
      "POST",
      &self.config.api_url("/api/v1/auth/local/login"),
      &headers,
      Some(request.to_string().into_bytes()),
    )
    .await
    .map_err(AuthError::Network)?;
    if response.status != 200 {
      let code = serde_json::from_slice::<teamcraft_proto::ErrorBody>(&response.body)
        .map(|body| body.error_code.as_str().to_owned())
        .unwrap_or_else(|_| format!("http_{}", response.status));
      return Err(AuthError::Denied(code));
    }
    let granted: teamcraft_proto::LocalLoginResponse = serde_json::from_slice(&response.body)
      .map_err(|error| AuthError::InvalidResponse(error.to_string()))?;
    let session = LocalSession {
      token: granted.access_token.clone(),
      expires_at_ms: granted.expires_at.0,
    };
    if let Ok(text) = serde_json::to_string(&session) {
      session_set(LOCAL_SESSION_KEY, &text);
    }
    *self.local.borrow_mut() = Some(session);
    Ok(())
  }

  async fn access_token(&self) -> Result<String, AuthError> {
    let local = self.local.borrow().clone();
    if let Some(session) = local {
      if session.expires_at_ms <= now_ms() {
        *self.local.borrow_mut() = None;
        session_remove(LOCAL_SESSION_KEY);
        return Err(AuthError::NotSignedIn);
      }
      return Ok(session.token);
    }
    let needs_refresh = match self.tokens.borrow().as_ref() {
      None => return Err(AuthError::NotSignedIn),
      Some(tokens) => tokens.expires_at_ms - REFRESH_MARGIN_MS <= now_ms(),
    };
    if needs_refresh {
      self.refresh().await?;
    }
    let tokens = self.tokens.borrow();
    tokens
      .as_ref()
      .map(|tokens| tokens.access.expose_secret().to_owned())
      .ok_or(AuthError::NotSignedIn)
  }

  fn store(&self, response: &oauth2::basic::BasicTokenResponse) {
    let lifetime_ms = response
      .expires_in()
      .map_or(60_000, |duration| duration.as_millis() as i64);
    let previous_refresh = self
      .tokens
      .borrow()
      .as_ref()
      .and_then(|tokens| tokens.maybe_refresh.clone());
    let maybe_refresh = response
      .refresh_token()
      .map(|token| SecretString::from(token.secret().clone()))
      .or(previous_refresh);
    self.tokens.replace(Some(Tokens {
      access: SecretString::from(response.access_token().secret().clone()),
      maybe_refresh,
      expires_at_ms: now_ms() + lifetime_ms,
    }));
  }

  /// The sign-in settings: the development overrides if both are set, else the server's answer
  /// (the page was served by this server, so it is the one to sign in to).
  async fn sign_in(&self) -> Result<SignIn, AuthError> {
    if let Some(found) = self.sign_in.borrow().clone() {
      return Ok(found);
    }
    let found = match (&self.config.maybe_issuer, &self.config.maybe_client_id) {
      (Some(issuer), Some(client_id)) => SignIn {
        issuer: issuer.clone(),
        client_id: client_id.clone(),
        scopes: vec![OPENID_SCOPE.to_owned()],
      },
      _ => self.server_sign_in().await?,
    };
    self.sign_in.replace(Some(found.clone()));
    Ok(found)
  }

  async fn server_sign_in(&self) -> Result<SignIn, AuthError> {
    let headers = [("Accept".to_owned(), "application/json".to_owned())];
    let response = fetch("GET", &self.config.api_url(SERVER_INFO_PATH), &headers, None)
      .await
      .map_err(AuthError::Network)?;
    if response.status != 200 {
      return Err(AuthError::Network(format!(
        "server-info returned HTTP {}",
        response.status
      )));
    }
    let info: ServerInfoResponse = serde_json::from_slice(&response.body)
      .map_err(|error| AuthError::InvalidResponse(error.to_string()))?;
    if info.product != SERVER_INFO_PRODUCT
      || info.sign_in.issuer.is_empty()
      || info.sign_in.client_id.is_empty()
    {
      return Err(AuthError::InvalidResponse("not a TeamCraft server".to_owned()));
    }
    let mut scopes = vec![OPENID_SCOPE.to_owned()];
    scopes.extend(
      info
        .sign_in
        .scopes
        .into_iter()
        .filter(|scope| scope != OPENID_SCOPE),
    );
    Ok(SignIn {
      issuer: info.sign_in.issuer,
      client_id: info.sign_in.client_id,
      scopes,
    })
  }

  async fn discovery(&self) -> Result<Discovery, AuthError> {
    if let Some(found) = self.discovery.borrow().clone() {
      return Ok(found);
    }
    let sign_in = self.sign_in().await?;
    let url = format!(
      "{}/.well-known/openid-configuration",
      sign_in.issuer.trim_end_matches('/')
    );
    let response = fetch("GET", &url, &[], None).await.map_err(AuthError::Network)?;
    if response.status != 200 {
      return Err(AuthError::Network(format!(
        "discovery returned HTTP {}",
        response.status
      )));
    }
    let found: Discovery = serde_json::from_slice(&response.body)
      .map_err(|error| AuthError::InvalidResponse(error.to_string()))?;
    // The page navigates to these endpoints: https only (a `javascript:` one would run in the
    // page), and the document must be the issuer's own (RFC 8414 §3.3).
    let mut endpoints = vec![
      found.authorization_endpoint.as_str(),
      found.token_endpoint.as_str(),
    ];
    endpoints.extend(found.maybe_end_session_endpoint.as_deref());
    let policy = if cfg!(debug_assertions) {
      HttpPolicy::Allowed
    } else {
      HttpPolicy::LoopbackOnly
    };
    check_discovery(&sign_in.issuer, &found.issuer, &endpoints, policy)
      .map_err(|error| AuthError::InvalidResponse(error.to_string()))?;
    self.discovery.replace(Some(found.clone()));
    Ok(found)
  }

  fn client(
    &self,
    discovery: &Discovery,
    redirect_uri: &str,
    sign_in: &SignIn,
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
    let invalid = |error: oauth2::url::ParseError| AuthError::InvalidResponse(error.to_string());
    Ok(
      BasicClient::new(ClientId::new(sign_in.client_id.clone()))
        .set_auth_uri(AuthUrl::new(discovery.authorization_endpoint.clone()).map_err(invalid)?)
        .set_token_uri(TokenUrl::new(discovery.token_endpoint.clone()).map_err(invalid)?)
        .set_redirect_uri(RedirectUrl::new(redirect_uri.to_owned()).map_err(invalid)?),
    )
  }
}

impl Authenticator for BrowserAuth {
  fn restore(&self) -> LocalFuture<'_, Result<bool, AuthError>> {
    Box::pin(self.restore_inner())
  }

  fn access_token(&self) -> LocalFuture<'_, Result<String, AuthError>> {
    Box::pin(BrowserAuth::access_token(self))
  }

  fn login(&self) -> LocalFuture<'_, Result<(), AuthError>> {
    Box::pin(async move {
      self.begin_login().await?;
      std::future::pending::<()>().await;
      Ok(())
    })
  }

  fn login_local(
    &self,
    login: String,
    password: String,
    maybe_totp: Option<String>,
  ) -> LocalFuture<'_, Result<(), AuthError>> {
    Box::pin(self.login_local_inner(login, password, maybe_totp))
  }

  fn logout(&self) -> LocalFuture<'_, Result<(), AuthError>> {
    Box::pin(async move {
      let local = self.local.borrow_mut().take();
      session_remove(LOCAL_SESSION_KEY);
      if let Some(session) = local {
        let headers = [("Authorization".to_owned(), format!("Bearer {}", session.token))];
        if let Err(error) = fetch(
          "POST",
          &self.config.api_url("/api/v1/auth/local/logout"),
          &headers,
          None,
        )
        .await
        {
          tracing::warn!(%error, "could not end the local session on the server");
        }
        return Ok(());
      }
      let maybe_refresh = self.tokens.replace(None).and_then(|tokens| tokens.maybe_refresh);
      local_remove(SIGNED_IN_HINT_KEY);
      session_remove(AUTOLOGIN_TRIED_KEY);
      // Ending the provider session as well is what makes "sign out" mean something on shared
      // machines. A direct request with the refresh token does it without a confirmation page.
      if let (Some(refresh), Ok(discovery), Ok(sign_in)) =
        (maybe_refresh, self.discovery().await, self.sign_in().await)
        && let Some(endpoint) = discovery.maybe_end_session_endpoint
      {
        let body = format!(
          "client_id={}&refresh_token={}",
          encode(&sign_in.client_id),
          encode(refresh.expose_secret())
        );
        let headers = [(
          "Content-Type".to_owned(),
          "application/x-www-form-urlencoded".to_owned(),
        )];
        match fetch("POST", &endpoint, &headers, Some(body.into_bytes())).await {
          Ok(response) if response.status < 300 => {},
          Ok(response) => tracing::warn!(status = response.status, "the provider did not end the session"),
          Err(error) => tracing::warn!(%error, "could not reach the provider to end the session"),
        }
      }
      Ok(())
    })
  }

  fn api_request(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<String>,
  ) -> LocalFuture<'_, Result<ApiResponse, AuthError>> {
    let url = self.config.api_url(path);
    Box::pin(async move {
      let token = self.access_token().await?;
      let mut headers = vec![
        ("Authorization".to_owned(), format!("Bearer {token}")),
        ("Accept".to_owned(), "application/json".to_owned()),
      ];
      if maybe_body.is_some() {
        headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
      }
      let response = fetch(
        method.as_str(),
        &url,
        &headers,
        maybe_body.map(String::into_bytes),
      )
      .await
      .map_err(AuthError::Network)?;
      Ok(ApiResponse {
        status: response.status,
        body: String::from_utf8_lossy(&response.body).into_owned(),
      })
    })
  }

  fn api_request_bytes(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<BinaryBody>,
  ) -> LocalFuture<'_, Result<ApiBytes, AuthError>> {
    let url = self.config.api_url(path);
    Box::pin(async move {
      let token = self.access_token().await?;
      let mut headers = vec![("Authorization".to_owned(), format!("Bearer {token}"))];
      if let Some(body) = &maybe_body {
        headers.push(("Content-Type".to_owned(), body.content_type.clone()));
      }
      let response = fetch(method.as_str(), &url, &headers, maybe_body.map(|body| body.bytes))
        .await
        .map_err(AuthError::Network)?;
      Ok(ApiBytes {
        status: response.status,
        maybe_content_type: response.maybe_content_type,
        body: response.body,
      })
    })
  }
}

/// HTTP client for the `oauth2` crate on top of `fetch`.
async fn http_client(request: oauth2::HttpRequest) -> Result<oauth2::HttpResponse, HttpError> {
  let headers: Vec<(String, String)> = request
    .headers()
    .iter()
    .filter_map(|(name, value)| {
      value
        .to_str()
        .ok()
        .map(|value| (name.to_string(), value.to_owned()))
    })
    .collect();
  let response = fetch(
    request.method().as_str(),
    &request.uri().to_string(),
    &headers,
    Some(request.body().clone()),
  )
  .await
  .map_err(HttpError)?;
  oauth2::http::Response::builder()
    .status(response.status)
    .body(response.body)
    .map_err(|error| HttpError(error.to_string()))
}

type TokenError =
  RequestTokenError<HttpError, oauth2::StandardErrorResponse<oauth2::basic::BasicErrorResponseType>>;

fn map_token_error(error: TokenError) -> AuthError {
  match error {
    RequestTokenError::ServerResponse(response) => {
      let description = response
        .error_description()
        .cloned()
        .unwrap_or_else(|| response.error().to_string());
      AuthError::Denied(description)
    },
    RequestTokenError::Request(error) => AuthError::Network(error.to_string()),
    RequestTokenError::Parse(error, _) => AuthError::InvalidResponse(error.to_string()),
    RequestTokenError::Other(message) => AuthError::InvalidResponse(message),
  }
}

fn redirect_uri() -> Result<String, AuthError> {
  let origin = web_sys::window()
    .and_then(|window| window.location().origin().ok())
    .ok_or_else(|| AuthError::Unsupported("no browser location".to_owned()))?;
  Ok(format!("{origin}/"))
}

fn navigate(url: &str) -> Result<(), AuthError> {
  web_sys::window()
    .ok_or_else(|| AuthError::Unsupported("no window".to_owned()))?
    .location()
    .set_href(url)
    .map_err(|_| AuthError::Unsupported("navigation was blocked".to_owned()))
}

fn encode(value: &str) -> String {
  String::from(js_sys::encode_uri_component(value))
}

/// Query parameters of the current page as pairs.
fn location_params() -> std::collections::HashMap<String, String> {
  let search = web_sys::window()
    .and_then(|window| window.location().search().ok())
    .unwrap_or_default();
  web_sys::UrlSearchParams::new_with_str(&search)
    .map(|params| {
      let mut map = std::collections::HashMap::new();
      let entries = js_sys::try_iter(&params).ok().flatten();
      for entry in entries.into_iter().flatten().flatten() {
        let pair = js_sys::Array::from(&entry);
        if let (Some(key), Some(value)) = (pair.get(0).as_string(), pair.get(1).as_string()) {
          map.insert(key, value);
        }
      }
      map
    })
    .unwrap_or_default()
}

/// Removes the OIDC response parameters from the address bar.
fn clean_location() {
  if let Some(window) = web_sys::window()
    && let Ok(history) = window.history()
  {
    let path = window.location().pathname().unwrap_or_else(|_| "/".to_owned());
    if history
      .replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&path))
      .is_err()
    {
      tracing::debug!("could not clean the address bar");
    }
  }
}

fn session_get(key: &str) -> Option<String> {
  web_sys::window()?
    .session_storage()
    .ok()
    .flatten()?
    .get_item(key)
    .ok()
    .flatten()
}

fn session_set(key: &str, value: &str) {
  if let Some(Some(storage)) = web_sys::window().and_then(|window| window.session_storage().ok())
    && storage.set_item(key, value).is_err()
  {
    tracing::debug!(key, "could not write sessionStorage");
  }
}

fn session_remove(key: &str) {
  if let Some(Some(storage)) = web_sys::window().and_then(|window| window.session_storage().ok()) {
    drop(storage.remove_item(key));
  }
}

fn local_get(key: &str) -> Option<String> {
  crate::settings::load_string(key)
}

fn local_set(key: &str, value: &str) {
  if let Some(Some(storage)) = web_sys::window().and_then(|window| window.local_storage().ok()) {
    drop(storage.set_item(key, value));
  }
}

fn local_remove(key: &str) {
  if let Some(Some(storage)) = web_sys::window().and_then(|window| window.local_storage().ok()) {
    drop(storage.remove_item(key));
  }
}
