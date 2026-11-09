//! OIDC authorization code flow with PKCE in the browser (redirect flow).
//!
//! The page navigates to the identity provider and comes back to `<origin>/` with `?code=…&state=…`.
//! Only the PKCE verifier and the `state` survive the redirect, in `sessionStorage`. The tokens are
//! sealed in IndexedDB ([`super::token_vault`]), so a reload restores the session with a
//! refresh-token grant; only without saved tokens, or when the provider refuses them, does it sign
//! in again through the provider's session (a redirect round trip without a password prompt).

use std::cell::RefCell;
use std::sync::Arc;

use oauth2::{
  AuthUrl, AuthorizationCode, ClientId, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RefreshToken,
  RequestTokenError, TokenResponse, TokenUrl,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use teamcraft_client_api::oidc_login::{
  AuthorizationErrorKind, LoginRequest, OidcClient, OidcTokenResponse, answer_subject, authorization_url,
  oidc_client, shown_error_code,
};
use teamcraft_client_api::server_entry::{HttpPolicy, SERVER_INFO_PATH, check_discovery};
use teamcraft_core::api::dpop::{
  Authorization, DPOP_ALGORITHM_FOR_DISCOVERY, DPOP_HEADER, DpopAnswer, DpopProofError, DpopProver,
  DpopSendError, ProofFallback, TokenEndpointAttempt, clock_offset_of, is_from_a_cache, signed_headers,
  with_token_endpoint_proof,
};
use teamcraft_crypto::dpop::{JwkThumbprint, token_bound_key};
use teamcraft_proto::{ErrorBody, ErrorCode, GetMeResponse, SERVER_INFO_PRODUCT, ServerInfoResponse};

use super::fetch::{FetchResponse, fetch, fetch_uncached};
use super::token_vault::{self, CachedAccount, RefreshPlan, Tokens};
use super::{
  ApiBytes, ApiMethod, ApiResponse, AppConfig, AuthError, Authenticator, BinaryBody, LocalFuture, SignInLook,
};
use crate::clock::now_ms;
use crate::route::Route;

const PENDING_KEY: &str = "teamcraft.oidc.pending";
const SIGNED_IN_HINT_KEY: &str = "teamcraft.signed_in";
const AUTOLOGIN_TRIED_KEY: &str = "teamcraft.autologin_tried";
/// The account last signed in with in this browser ([`StoredLoginHint`]), prefilled on the
/// provider's login page (issue #448). The page belongs to one server, so the key needs no origin.
const LOGIN_HINT_KEY: &str = "teamcraft.login_hint";
/// The bearer token of a local account, for the life of the tab.
const LOCAL_SESSION_KEY: &str = "teamcraft.local_session";
/// HTTP status of an API call whose access token the server refused.
const UNAUTHORIZED: u16 = 401;
/// The scope every sign-in asks for, also with development overrides.
const OPENID_SCOPE: &str = "openid";

pub struct BrowserAuth {
  config: AppConfig,
  /// Set while a local account (guest) is signed in instead of an identity provider account.
  local: RefCell<Option<LocalSession>>,
  tokens: RefCell<Option<Tokens>>,
  /// The ID token of the provider's last answer; memory only, a refresh brings a new one.
  maybe_id_token: RefCell<Option<String>>,
  sign_in: RefCell<Option<SignIn>>,
  discovery: RefCell<Option<Discovery>>,
  /// The issuer and client the tokens in memory were saved for, while they come from the cached start
  /// and the server hasn't been asked yet. The server is not trusted: its answer may name another
  /// provider, and a refresh token only ever goes to the provider it came from.
  maybe_cached_provider: RefCell<Option<(String, String)>>,
  /// The device's DPoP key with what is known about this server, once it is read or made.
  maybe_prover: RefCell<Option<Arc<DpopProver>>>,
  /// The same key with what is known about the identity provider.
  maybe_provider_prover: RefCell<Option<Arc<DpopProver>>>,
}

/// How to sign in: the server's settings, or the development overrides of [`AppConfig`].
#[derive(Clone, Debug)]
struct SignIn {
  issuer: String,
  client_id: String,
  scopes: Vec<String>,
  /// Interactive sign-ins ask for the provider's login page (the server's setting).
  is_login_page_always_shown: bool,
  /// What server-info said about proofs: `DPoP-Nonce` means the server would check them on this
  /// host. Without it a bound token would be refused, so the sign-in stays unbound.
  maybe_server_nonce: Option<String>,
  /// The server's clock minus this browser's, by the same answer: the provider's `Date` is often
  /// out of reach, and a proof with a wrong date would be refused there.
  maybe_clock_offset_secs: Option<i64>,
}

/// What a token request for a session that isn't bound is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnboundRequest {
  SignIn,
  /// Worth a proof until the provider refused one in this run.
  Refresh,
}

/// The login hint with the identity provider it belongs to: a server that switched providers
/// gets no hint of the old one.
#[derive(Serialize, Deserialize)]
struct StoredLoginHint {
  issuer: String,
  client_id: String,
  hint: String,
}

/// The session of a local account: the server issues the token, there is no refresh.
#[derive(Clone, Serialize, Deserialize)]
struct LocalSession {
  token: String,
  expires_at_ms: i64,
  /// The account the server confirmed for this session, for the next start of the tab (issue #778).
  #[serde(default, rename = "account", skip_serializing_if = "Option::is_none")]
  maybe_account: Option<GetMeResponse>,
}

#[derive(Clone, Debug, Deserialize)]
struct Discovery {
  issuer: String,
  authorization_endpoint: String,
  token_endpoint: String,
  #[serde(default, rename = "end_session_endpoint")]
  maybe_end_session_endpoint: Option<String>,
  #[serde(default, rename = "revocation_endpoint")]
  maybe_revocation_endpoint: Option<String>,
  /// RFC 9449 §5.1: the provider takes proofs signed so.
  #[serde(default)]
  dpop_signing_alg_values_supported: Vec<String>,
}

/// What must survive the redirect to the identity provider.
#[derive(Serialize, Deserialize)]
struct Pending {
  state: String,
  verifier: String,
  redirect_uri: String,
  /// The app's path when sign-in began (`/c/123`), restored afterwards. Only route paths, which
  /// hold nothing but public IDs (route.rs).
  #[serde(default, rename = "return_path")]
  maybe_return_path: Option<String>,
  /// The request's login page and language, so an automatic retry asks the same (issue #1063).
  #[serde(default)]
  is_login_page_shown: bool,
  #[serde(default, rename = "locale")]
  maybe_locale: Option<String>,
  /// Automatic retries of this sign-in so far.
  #[serde(default)]
  retries: u32,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct HttpError(String);

impl From<DpopProofError> for HttpError {
  fn from(error: DpopProofError) -> Self {
    Self(error.to_string())
  }
}

impl BrowserAuth {
  pub fn new(config: AppConfig) -> Self {
    Self {
      config,
      local: RefCell::new(None),
      tokens: RefCell::new(None),
      maybe_id_token: RefCell::new(None),
      sign_in: RefCell::new(None),
      discovery: RefCell::new(None),
      maybe_cached_provider: RefCell::new(None),
      maybe_prover: RefCell::new(None),
      maybe_provider_prover: RefCell::new(None),
    }
  }

  /// The cached account with its session in memory, without the network. Tokens that ran out are
  /// taken too: asking for the access token then refreshes them when it can.
  async fn cached_start(&self) -> Option<GetMeResponse> {
    let params = location_params();
    if params.contains_key("code") || params.contains_key("error") {
      return None;
    }
    if let Some(session) = stored_local_session() {
      let account = session.maybe_account.clone()?;
      *self.local.borrow_mut() = Some(session);
      return Some(account);
    }
    local_get(SIGNED_IN_HINT_KEY)?;
    let cached = token_vault::load_account().await?;
    // Read only: a stale record says nothing about the tokens, which stay for the long way.
    let tokens = token_vault::peek(&cached.issuer, &cached.client_id).await?;
    if tokens.maybe_subject.as_deref() != Some(cached.subject.as_str()) {
      return None;
    }
    self.tokens.replace(Some(tokens));
    self
      .maybe_cached_provider
      .replace(Some((cached.issuer, cached.client_id)));
    Some(cached.account)
  }

  /// Saves `account` next to the session it was confirmed for: a local account's in this tab's
  /// session, else in the vault, unless the user signed out meanwhile.
  async fn keep_account(&self, account: &GetMeResponse) {
    if let Some(session) = self.local.borrow_mut().as_mut() {
      session.maybe_account = Some(account.clone());
      if let Ok(text) = serde_json::to_string(session) {
        session_set(LOCAL_SESSION_KEY, &text);
      }
      return;
    }
    let maybe_subject = self
      .tokens
      .borrow()
      .as_ref()
      .and_then(|tokens| tokens.maybe_subject.clone());
    let Some(subject) = maybe_subject else {
      return;
    };
    let Ok(sign_in) = self.sign_in().await else {
      return;
    };
    // Under the refresh lock, like the tokens: a sign-out in another tab either sees this record
    // and removes it, or removed the hint before this looks at it.
    let _turn = token_vault::refresh_turn().await;
    if local_get(SIGNED_IN_HINT_KEY).is_none() {
      return;
    }
    token_vault::save_account(&CachedAccount {
      issuer: sign_in.issuer,
      client_id: sign_in.client_id,
      subject,
      account: account.clone(),
    })
    .await;
  }

  async fn restore_inner(&self) -> Result<bool, AuthError> {
    if let Some(session) = stored_local_session() {
      *self.local.borrow_mut() = Some(session);
      return Ok(true);
    }
    session_remove(LOCAL_SESSION_KEY);
    let params = location_params();
    if let Some(error) = params.get("error") {
      clean_location_to(None);
      return self.refused(error, params.get("state")).await;
    }
    if let (Some(code), Some(state)) = (params.get("code"), params.get("state")) {
      let outcome = self.finish_login(code, state).await;
      // Back to where sign-in began; a failed sign-in stays on the start page.
      let maybe_return_path = outcome.as_ref().ok().and_then(Option::as_deref);
      clean_location_to(maybe_return_path);
      return outcome.map(|_| true);
    }
    if local_get(SIGNED_IN_HINT_KEY).is_none() {
      // Signed out (perhaps in another tab while this one refreshed): nothing may stay behind.
      token_vault::forget().await;
      return Ok(false);
    }
    // Signed in before and the tab was reloaded: the saved tokens, if the provider still takes them.
    match self.refresh().await {
      Ok(()) => return Ok(true),
      // Nothing saved, or the session was revoked or expired: the provider decides below.
      // (Reported where it was decided, in `refresh_when`.)
      Err(AuthError::NotSignedIn) => {},
      // Offline or the server is down: the saved tokens stay for the next start.
      Err(error) => return Err(error),
    }
    // Go through the provider again, once, without asking (and without its login page: the user
    // stays signed in as long as the provider's session lasts).
    if session_get(AUTOLOGIN_TRIED_KEY).is_none() {
      session_set(AUTOLOGIN_TRIED_KEY, "1");
      // Started from the cached account, the app is on screen and may be in use.
      #[cfg(target_arch = "wasm32")]
      crate::web_core::save_before_leaving().await;
      self.begin_login(None).await?;
      std::future::pending::<()>().await;
    }
    Ok(false)
  }

  /// The identity provider refused the sign-in in its redirect (issue #1063). `already_logged_in`
  /// of this tab's own request goes to the provider once more, with a fresh `state` and verifier
  /// and the same login page; everything else ends on the sign-in screen with the code.
  async fn refused(&self, error: &str, maybe_state: Option<&String>) -> Result<bool, AuthError> {
    let code = shown_error_code(error);
    let kind = AuthorizationErrorKind::from_code(&code);
    let maybe_pending = session_get(PENDING_KEY).and_then(|json| serde_json::from_str::<Pending>(&json).ok());
    session_remove(PENDING_KEY);
    // Only a redirect for the request this tab sent is retried, never a link someone crafted.
    if let Some(pending) = maybe_pending.filter(|pending| Some(&pending.state) == maybe_state)
      && kind.is_retried(pending.retries)
    {
      tracing::warn!(error_code = %code, ?kind, "the identity provider refused the sign-in, trying again");
      let sign_in = self.sign_in().await?;
      let maybe_hint = stored_login_hint(&sign_in);
      let request = LoginRequest {
        is_login_page_shown: pending.is_login_page_shown,
        maybe_login_hint: maybe_hint.as_deref(),
        maybe_locale: pending.maybe_locale.as_deref(),
      };
      self
        .go_to_provider(&sign_in, &request, pending.maybe_return_path, pending.retries + 1)
        .await?;
      std::future::pending::<()>().await;
    }
    tracing::warn!(error_code = %code, ?kind, "the identity provider refused the sign-in");
    Err(AuthError::AuthorizationRefused(code))
  }

  /// Goes to the identity provider. `maybe_look` is set for a sign-in the user started: it then
  /// asks for the login page with the last account and the app's language (issue #448); a silent
  /// renewal after a reload (`None`) sends none of that.
  async fn begin_login(&self, maybe_look: Option<&SignInLook>) -> Result<(), AuthError> {
    let sign_in = self.sign_in().await?;
    let maybe_hint = stored_login_hint(&sign_in);
    let page_language = document_language();
    let request = login_request(
      &sign_in,
      maybe_look,
      maybe_hint.as_deref(),
      page_language.as_deref(),
    );
    self
      .go_to_provider(&sign_in, &request, current_route_path(), 0)
      .await
  }

  /// Navigates to the provider's authorization endpoint with a fresh `state` and PKCE verifier,
  /// which wait in `sessionStorage` for the redirect back.
  async fn go_to_provider(
    &self,
    sign_in: &SignIn,
    request: &LoginRequest<'_>,
    maybe_return_path: Option<String>,
    retries: u32,
  ) -> Result<(), AuthError> {
    let discovery = self.discovery().await?;
    let redirect_uri = redirect_uri()?;
    let client = self.client(&discovery, &redirect_uri, sign_in)?;
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, state) = authorization_url(&client, &sign_in.scopes, challenge, request);
    let pending = Pending {
      state: state.secret().clone(),
      verifier: verifier.secret().clone(),
      redirect_uri,
      maybe_return_path,
      is_login_page_shown: request.is_login_page_shown,
      maybe_locale: request.maybe_locale.map(str::to_owned),
      retries,
    };
    let pending =
      serde_json::to_string(&pending).map_err(|error| AuthError::InvalidResponse(error.to_string()))?;
    session_set(PENDING_KEY, &pending);
    navigate(url.as_str())
  }

  /// Trades the code for tokens; the answer is the path to return to.
  async fn finish_login(&self, code: &str, state: &str) -> Result<Option<String>, AuthError> {
    let pending = session_get(PENDING_KEY).ok_or(AuthError::StateMismatch)?;
    session_remove(PENDING_KEY);
    let pending: Pending = serde_json::from_str(&pending).map_err(|_| AuthError::StateMismatch)?;
    if pending.state != state {
      return Err(AuthError::StateMismatch);
    }
    let sign_in = self.sign_in().await?;
    let discovery = self.discovery().await?;
    let client = self.client(&discovery, &pending.redirect_uri, &sign_in)?;
    let maybe_prover = self
      .new_session_prover(&sign_in, &discovery, UnboundRequest::SignIn)
      .await;
    let http = |request| token_request(maybe_prover.clone(), ProofFallback::OnRefusalOrNoAnswer, request);
    let response = client
      .exchange_code(AuthorizationCode::new(code.to_owned()))
      .set_pkce_verifier(PkceCodeVerifier::new(pending.verifier))
      .request_async(&http)
      .await
      .map_err(map_token_error)?;
    self
      .maybe_id_token
      .replace(response.extra_fields().maybe_id_token.clone());
    self.tokens.replace(Some(tokens_from(&response, None, None)));
    self.note_unbound_sign_in(&response).await;
    if let Some(hint) = response.extra_fields().login_hint() {
      remember_login_hint(&sign_in, hint);
    }
    local_set(SIGNED_IN_HINT_KEY, "1");
    session_remove(AUTOLOGIN_TRIED_KEY);
    let _turn = token_vault::refresh_turn().await;
    // Whoever signed in now: the cached account no longer stands, before the new tokens are saved.
    token_vault::forget_account().await;
    self.share_tokens(&sign_in).await;
    Ok(pending.maybe_return_path.filter(|path| is_return_path(path)))
  }

  /// Renews the access token, one tab of the browser at a time: tokens another tab got meanwhile
  /// are taken over instead (refresh-token rotation). `NotSignedIn` when there is nothing to renew
  /// with or the provider refused it (expired or revoked).
  async fn refresh(&self) -> Result<(), AuthError> {
    self.refresh_when(false).await
  }

  /// `is_forced`: asks the provider even if the tokens are still fresh.
  async fn refresh_when(&self, is_forced: bool) -> Result<(), AuthError> {
    let sign_in = self.sign_in().await?;
    self.drop_tokens_of_another_provider(&sign_in);
    let _turn = token_vault::refresh_turn().await;
    let maybe_saved = match token_vault::load(&sign_in.issuer, &sign_in.client_id).await {
      Ok(maybe_saved) => maybe_saved,
      // This tab's own tokens still do; another tab's newer ones are looked for next time.
      Err(_) if self.tokens.borrow().is_some() => None,
      // On start an unreadable vault is not a missing sign-in: the tokens stay for the next try,
      // and the user isn't sent to the provider's login page for it.
      Err(error) => {
        super::report_lost_sign_in("web: the saved sign-in can't be read");
        return Err(AuthError::Network(error.to_string()));
      },
    };
    let plan = token_vault::plan_refresh(self.tokens.borrow().as_ref(), maybe_saved, now_ms(), is_forced);
    let (refresh, maybe_subject, maybe_bound_key, is_shared) = match plan {
      RefreshPlan::Adopt(tokens) => {
        self.tokens.replace(Some(tokens));
        return Ok(());
      },
      RefreshPlan::Refresh {
        token,
        maybe_subject,
        maybe_bound_key,
        is_shared,
      } => (token, maybe_subject, maybe_bound_key, is_shared),
      RefreshPlan::NotSignedIn => {
        // Signed in before in this browser, and nothing saved any more: lost, not signed out.
        if local_get(SIGNED_IN_HINT_KEY).is_some() {
          super::report_lost_sign_in("web: signed in before, no saved sign-in");
        }
        return Err(AuthError::NotSignedIn);
      },
    };
    let discovery = self.discovery().await?;
    let client = self.client(&discovery, &redirect_uri()?, &sign_in)?;
    let (maybe_prover, fallback) = match maybe_bound_key {
      Some(bound) => match self.provider_prover_of_key(&bound).await {
        Ok(prover) => (Some(prover), ProofFallback::None),
        Err(AuthError::KeyLost) => {
          // The refresh token only works with that key. Nothing can bring it back.
          super::report_lost_sign_in("web: the DPoP key of this sign-in is gone");
          self.tokens.replace(None);
          if is_shared {
            token_vault::forget().await;
          }
          return Err(AuthError::KeyLost);
        },
        // The key's store can't be read now, which says nothing about the session.
        Err(error) => return Err(error),
      },
      None => (
        self
          .new_session_prover(&sign_in, &discovery, UnboundRequest::Refresh)
          .await,
        ProofFallback::OnRefusalOrNoAnswer,
      ),
    };
    let http = |request| token_request(maybe_prover.clone(), fallback, request);
    match client
      .exchange_refresh_token(&RefreshToken::new(refresh.expose_secret().to_owned()))
      .request_async(&http)
      .await
    {
      Ok(response) => {
        self
          .tokens
          .replace(Some(tokens_from(&response, Some(refresh), maybe_subject)));
        if let Some(id_token) = &response.extra_fields().maybe_id_token {
          self.maybe_id_token.replace(Some(id_token.clone()));
        }
        if is_shared {
          self.share_tokens(&sign_in).await;
        }
        Ok(())
      },
      Err(error) => {
        // Only an expired, revoked or reused refresh token (`invalid_grant`) ends the session;
        // other answers (`temporarily_unavailable`, `server_error`) and network errors keep it for
        // the next try (issue #667).
        if is_session_over(&error) {
          super::report_lost_sign_in("web: the provider refused the refresh token");
          self.tokens.replace(None);
          if is_shared {
            token_vault::forget().await;
          }
          return Err(AuthError::NotSignedIn);
        }
        Err(map_refresh_error(error))
      },
    }
  }

  /// The server names another provider than the one the cached start's tokens came from: they are
  /// no use there, and must not reach it.
  fn drop_tokens_of_another_provider(&self, sign_in: &SignIn) {
    let is_other = is_other_provider(
      self.maybe_cached_provider.borrow().as_ref(),
      &sign_in.issuer,
      &sign_in.client_id,
    );
    self.maybe_cached_provider.replace(None);
    if is_other {
      tracing::warn!("the server names another identity provider than the saved sign-in's");
      self.tokens.replace(None);
    }
  }

  /// The server refused the access token (the session ended, e.g. by a back-channel logout): the
  /// saved tokens go, so no reload brings them back, and the next call renews the session at the
  /// provider or ends it. A local account's session has nothing saved.
  async fn on_unauthorized(&self) {
    if self.local.borrow().is_some() {
      return;
    }
    if let Some(tokens) = self.tokens.borrow_mut().as_mut() {
      tokens.expires_at_ms = 0;
    }
    let _turn = token_vault::refresh_turn().await;
    token_vault::forget().await;
  }

  /// Saves this tab's tokens for reloads and the other tabs, unless the user signed out meanwhile.
  async fn share_tokens(&self, sign_in: &SignIn) {
    if local_get(SIGNED_IN_HINT_KEY).is_none() {
      return;
    }
    let Some(tokens) = self.tokens.borrow().as_ref().cloned() else {
      return;
    };
    token_vault::save(&sign_in.issuer, &sign_in.client_id, &tokens).await;
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
      maybe_account: None,
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
      Some(tokens) => !tokens.is_fresh(now_ms()),
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
        is_login_page_always_shown: true,
        maybe_server_nonce: None,
        maybe_clock_offset_secs: None,
      },
      _ => self.server_sign_in().await?,
    };
    self.sign_in.replace(Some(found.clone()));
    Ok(found)
  }

  async fn server_sign_in(&self) -> Result<SignIn, AuthError> {
    let headers = [("Accept".to_owned(), "application/json".to_owned())];
    // Past the browser's cache: its `Date` sets the clock, and its `DPoP-Nonce` decides the binding.
    let response = fetch_uncached(&self.config.api_url(SERVER_INFO_PATH), &headers)
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
      maybe_server_nonce: response.maybe_dpop_nonce.clone(),
      maybe_clock_offset_secs: fresh_clock_offset(&response),
      issuer: info.sign_in.issuer,
      client_id: info.sign_in.client_id,
      scopes,
      is_login_page_always_shown: info.sign_in.is_login_page_always_shown,
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
    endpoints.extend(found.maybe_revocation_endpoint.as_deref());
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

  /// The DPoP key with what is known about this server, read once. `Ok(None)` only if the store was
  /// read and has no key; a store that can't be read is a network-like error, retried, that decides
  /// nothing about the session.
  async fn prover(&self) -> Result<Option<Arc<DpopProver>>, AuthError> {
    if let Some(prover) = self.maybe_prover.borrow().as_ref() {
      return Ok(Some(Arc::clone(prover)));
    }
    let maybe_found = stored_prover().await.map_err(AuthError::Network)?;
    if let Some(found) = &maybe_found {
      self.maybe_prover.replace(Some(Arc::clone(found)));
    }
    Ok(maybe_found)
  }

  /// What proves the key at the token endpoint for a session that isn't bound yet: only when the
  /// provider takes proofs and the server would check them. Otherwise, or when the key can't be
  /// read or made, the session stays unbound, as it always was.
  async fn new_session_prover(
    &self,
    sign_in: &SignIn,
    discovery: &Discovery,
    request: UnboundRequest,
  ) -> Option<Arc<DpopProver>> {
    let takes_proofs = discovery
      .dpop_signing_alg_values_supported
      .iter()
      .any(|algorithm| algorithm == DPOP_ALGORITHM_FOR_DISCOVERY);
    let server_nonce = sign_in.maybe_server_nonce.as_deref().filter(|_| takes_proofs)?;
    let was_refused = self
      .maybe_provider_prover
      .borrow()
      .as_ref()
      .is_some_and(|provider| provider.has_refused_proof());
    if request == UnboundRequest::Refresh && was_refused {
      return None;
    }
    let prover = match self.prover().await {
      Ok(Some(prover)) => prover,
      Ok(None) => {
        let made = new_prover().await?;
        self.maybe_prover.replace(Some(Arc::clone(&made)));
        made
      },
      Err(error) => {
        tracing::warn!(%error, "could not read the DPoP key, the session stays unbound");
        return None;
      },
    };
    prover.remember_nonce(server_nonce);
    Some(self.provider_prover(&prover))
  }

  /// The provider's prover for the key a session is bound to. [`AuthError::KeyLost`] only if the
  /// store was read and doesn't have that key.
  async fn provider_prover_of_key(&self, bound: &JwkThumbprint) -> Result<Arc<DpopProver>, AuthError> {
    let prover = key_of_session(self.prover().await, bound)?;
    // The nonce server-info offered saves the first request its challenge.
    let sign_in = self.sign_in.borrow();
    if let Some(nonce) = sign_in
      .as_ref()
      .and_then(|found| found.maybe_server_nonce.as_deref())
    {
      prover.remember_nonce(nonce);
    }
    drop(sign_in);
    Ok(self.provider_prover(&prover))
  }

  /// The provider's prover of `prover`'s key, with the clock as far as it is known at this moment.
  fn provider_prover(&self, prover: &DpopProver) -> Arc<DpopProver> {
    if let Some(offset) = self
      .sign_in
      .borrow()
      .as_ref()
      .and_then(|found| found.maybe_clock_offset_secs)
    {
      prover.seed_clock(offset);
    }
    let maybe_known = self
      .maybe_provider_prover
      .borrow()
      .as_ref()
      .filter(|known| known.thumbprint() == prover.thumbprint())
      .map(Arc::clone);
    let provider = maybe_known.unwrap_or_else(|| {
      // In the browser the key is Web Crypto's and stays on this thread; `Arc` is the shared
      // interface's.
      #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
      let made = Arc::new(prover.for_another_party());
      self.maybe_provider_prover.replace(Some(Arc::clone(&made)));
      made
    });
    provider.seed_clock(prover.clock_offset());
    provider
  }

  /// A device with a key that signs in unbound is worth a line in the log: a server that withholds
  /// its nonce or a provider that refuses proofs looks like this. There is nothing the user could
  /// do about it, and under `optional` it works, so the app says nothing.
  async fn note_unbound_sign_in(&self, response: &OidcTokenResponse) {
    if token_bound_key(response.access_token().secret()).is_none()
      && matches!(self.prover().await, Ok(Some(_)))
    {
      tracing::warn!("this device has a DPoP key and signed in without binding the sign-in to it");
    }
  }

  /// A request with the current token, signed for a bound one, with what DPoP asks for: the answer's
  /// nonce and clock are kept, and a proof refused for them is made again once, never more.
  async fn authorized_fetch(
    &self,
    method: &str,
    url: &str,
    mut headers: Vec<(String, String)>,
    maybe_body: Option<Vec<u8>>,
  ) -> Result<FetchResponse, AuthError> {
    let token = self.access_token().await?;
    let proof_url = absolute_url(url);
    let maybe_prover = match Authorization::of(&token) {
      Authorization::Bearer => None,
      Authorization::Bound(_) => Some(self.prover().await?.ok_or(AuthError::KeyLost)?),
    };
    let first_headers = sign_headers(maybe_prover.as_deref(), &token, method, &proof_url).await?;
    headers.extend(first_headers);
    let first = fetch(method, url, &headers, maybe_body.clone())
      .await
      .map_err(AuthError::Network)?;
    let Some(prover) = maybe_prover else {
      return Ok(first);
    };
    let learned = prover.note_answer(dpop_answer(&first));
    let is_retried = first.status == UNAUTHORIZED
      && serde_json::from_slice::<ErrorBody>(&first.body)
        .is_ok_and(|body| learned.is_worth_a_retry(&body.error_code));
    if !is_retried {
      return Ok(first);
    }
    let second_headers = sign_headers(Some(&prover), &token, method, &proof_url).await?;
    headers.retain(|(name, _)| name != "Authorization" && name != DPOP_HEADER);
    headers.extend(second_headers);
    let second = fetch(method, url, &headers, maybe_body)
      .await
      .map_err(AuthError::Network)?;
    prover.note_answer(dpop_answer(&second));
    Ok(second)
  }

  /// The key of the session is gone: the tokens are of no use any more, and go.
  async fn forget_after_lost_key(&self) {
    self.tokens.replace(None);
    let _turn = token_vault::refresh_turn().await;
    token_vault::forget().await;
  }

  /// Ends the session at the identity provider: with the end-session request, and, if that is refused,
  /// by revoking the refresh token. The dev realm's Keycloak refuses end-session for a bound refresh
  /// token even with its proof, and revoking ends the session there as well.
  async fn end_provider_session(
    &self,
    discovery: &Discovery,
    client_id: &str,
    refresh: &SecretString,
    maybe_bound: Option<&JwkThumbprint>,
  ) {
    let maybe_prover = match maybe_bound {
      Some(bound) => self
        .provider_prover_of_key(bound)
        .await
        .inspect_err(|error| tracing::warn!(%error, "no proof for ending the session"))
        .ok(),
      None => None,
    };
    let client = encode(client_id);
    let token = encode(refresh.expose_secret());
    if let Some(endpoint) = &discovery.maybe_end_session_endpoint {
      let body = format!("client_id={client}&refresh_token={token}");
      match provider_post(maybe_prover.as_ref(), endpoint, &body).await {
        Ok(status) if status < 300 => return,
        Ok(status) => tracing::warn!(status, "the provider did not end the session"),
        Err(error) => tracing::warn!(%error, "could not reach the provider to end the session"),
      }
    }
    if let Some(endpoint) = &discovery.maybe_revocation_endpoint {
      let body = format!("client_id={client}&token={token}&token_type_hint=refresh_token");
      match provider_post(maybe_prover.as_ref(), endpoint, &body).await {
        Ok(status) if status < 300 => {},
        Ok(status) => tracing::warn!(status, "the provider did not revoke the sign-in"),
        Err(error) => tracing::warn!(%error, "could not reach the provider to revoke the sign-in"),
      }
    }
  }

  fn client(
    &self,
    discovery: &Discovery,
    redirect_uri: &str,
    sign_in: &SignIn,
  ) -> Result<OidcClient, AuthError> {
    let invalid = |error: oauth2::url::ParseError| AuthError::InvalidResponse(error.to_string());
    Ok(oidc_client(
      ClientId::new(sign_in.client_id.clone()),
      AuthUrl::new(discovery.authorization_endpoint.clone()).map_err(invalid)?,
      TokenUrl::new(discovery.token_endpoint.clone()).map_err(invalid)?,
      RedirectUrl::new(redirect_uri.to_owned()).map_err(invalid)?,
    ))
  }
}

impl Authenticator for BrowserAuth {
  fn restore(&self) -> LocalFuture<'_, Result<bool, AuthError>> {
    Box::pin(self.restore_inner())
  }

  fn cached_account(&self) -> LocalFuture<'_, Option<GetMeResponse>> {
    Box::pin(self.cached_start())
  }

  fn remember_account<'a>(&'a self, account: &'a GetMeResponse) -> LocalFuture<'a, ()> {
    Box::pin(self.keep_account(account))
  }

  fn forget_account(&self) -> LocalFuture<'_, ()> {
    Box::pin(async move {
      if let Some(session) = self.local.borrow_mut().as_mut() {
        session.maybe_account = None;
        if let Ok(text) = serde_json::to_string(session) {
          session_set(LOCAL_SESSION_KEY, &text);
        }
        return;
      }
      let _turn = token_vault::refresh_turn().await;
      token_vault::forget_account().await;
    })
  }

  fn access_token(&self) -> LocalFuture<'_, Result<String, AuthError>> {
    Box::pin(BrowserAuth::access_token(self))
  }

  fn id_token(&self) -> Option<String> {
    self.maybe_id_token.borrow().clone()
  }

  fn refresh_id_token(&self) -> LocalFuture<'_, ()> {
    Box::pin(async {
      if let Err(error) = self.refresh_when(true).await {
        tracing::debug!(%error, "no new ID token yet");
      }
    })
  }

  /// `None` if there is no key or its store can't be read now. The core then holds a bound token back
  /// and retries; whether the key is really gone is decided here, by [`BrowserAuth::prover`].
  fn dpop_prover(&self) -> LocalFuture<'_, Option<Arc<DpopProver>>> {
    Box::pin(async move {
      self
        .prover()
        .await
        .inspect_err(|error| tracing::warn!(%error, "could not read the DPoP key"))
        .ok()
        .flatten()
    })
  }

  fn login(&self, look: SignInLook) -> LocalFuture<'_, Result<(), AuthError>> {
    Box::pin(async move {
      self.begin_login(Some(&look)).await?;
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
      let maybe_tokens = self.tokens.replace(None);
      let maybe_bound = maybe_tokens
        .as_ref()
        .and_then(|tokens| token_bound_key(tokens.access.expose_secret()));
      let maybe_refresh = maybe_tokens.and_then(|tokens| tokens.maybe_refresh);
      let maybe_cached_provider = self.maybe_cached_provider.take();
      local_remove(SIGNED_IN_HINT_KEY);
      session_remove(AUTOLOGIN_TRIED_KEY);
      // Under the refresh lock: a refresh another tab runs right now saves its tokens before this
      // removes them, and every later one sees the hint gone and saves nothing.
      {
        let _turn = token_vault::refresh_turn().await;
        token_vault::forget().await;
      }
      // Ending the provider session as well is what makes "sign out" mean something on shared
      // machines. A direct request with the refresh token does it without a confirmation page.
      if let (Some(refresh), Ok(discovery), Ok(sign_in)) =
        (maybe_refresh, self.discovery().await, self.sign_in().await)
        && !is_other_provider(
          maybe_cached_provider.as_ref(),
          &sign_in.issuer,
          &sign_in.client_id,
        )
      {
        self
          .end_provider_session(&discovery, &sign_in.client_id, &refresh, maybe_bound.as_ref())
          .await;
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
      let mut headers = vec![("Accept".to_owned(), "application/json".to_owned())];
      if maybe_body.is_some() {
        headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
      }
      let outcome = self
        .authorized_fetch(method.as_str(), &url, headers, maybe_body.map(String::into_bytes))
        .await;
      if matches!(outcome, Err(AuthError::KeyLost)) {
        self.forget_after_lost_key().await;
      }
      let response = outcome?;
      if is_refused_token(response.status, &response.body) {
        self.on_unauthorized().await;
      }
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
      let mut headers = Vec::new();
      if let Some(body) = &maybe_body {
        headers.push(("Content-Type".to_owned(), body.content_type.clone()));
      }
      let outcome = self
        .authorized_fetch(method.as_str(), &url, headers, maybe_body.map(|body| body.bytes))
        .await;
      if matches!(outcome, Err(AuthError::KeyLost)) {
        self.forget_after_lost_key().await;
      }
      let response = outcome?;
      if is_refused_token(response.status, &response.body) {
        self.on_unauthorized().await;
      }
      Ok(ApiBytes {
        status: response.status,
        maybe_content_type: response.maybe_content_type,
        body: response.body,
      })
    })
  }
}

/// The tokens of a provider answer; a refresh answer without a new refresh token keeps
/// `maybe_previous_refresh` (no rotation), one that names no account `maybe_previous_subject`.
fn tokens_from(
  response: &OidcTokenResponse,
  maybe_previous_refresh: Option<SecretString>,
  maybe_previous_subject: Option<String>,
) -> Tokens {
  let lifetime_ms = response
    .expires_in()
    .map_or(60_000, |duration| duration.as_millis() as i64);
  let maybe_refresh = response
    .refresh_token()
    .map(|token| SecretString::from(token.secret().clone()))
    .or(maybe_previous_refresh);
  let access = response.access_token().secret();
  let maybe_subject =
    answer_subject(response.extra_fields().maybe_id_token.as_deref(), access).or(maybe_previous_subject);
  Tokens::new(
    SecretString::from(access.clone()),
    maybe_refresh,
    now_ms() + lifetime_ms,
    maybe_subject,
  )
}

/// HTTP client for the `oauth2` crate on top of `fetch`, with a DPoP proof when a key is given.
async fn token_request(
  maybe_prover: Option<Arc<DpopProver>>,
  fallback: ProofFallback,
  request: oauth2::HttpRequest,
) -> Result<oauth2::HttpResponse, HttpError> {
  let method = request.method().as_str().to_owned();
  let url = request.uri().to_string();
  let body = request.body().clone();
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
  let attempt = |maybe_proof: Option<String>| {
    let (method, url, headers, body) = (&method, &url, &headers, &body);
    async move { token_attempt(method, url, headers, body, maybe_proof).await }
  };
  match maybe_prover {
    Some(prover) => with_token_endpoint_proof(&prover, &url, fallback, attempt).await,
    None => attempt(None).await.map(|answer| answer.response),
  }
}

/// A form to the identity provider with a proof if there is a key; the answer's status. A nonce
/// challenge gets its one more try.
async fn provider_post(
  maybe_prover: Option<&Arc<DpopProver>>,
  url: &str,
  body: &str,
) -> Result<u16, HttpError> {
  let headers = [(
    "Content-Type".to_owned(),
    "application/x-www-form-urlencoded".to_owned(),
  )];
  let attempt = |maybe_proof: Option<String>| {
    let headers = &headers;
    async move { token_attempt("POST", url, headers, body.as_bytes(), maybe_proof).await }
  };
  let answer = match maybe_prover {
    Some(prover) => with_token_endpoint_proof(prover, url, ProofFallback::None, attempt).await,
    None => attempt(None).await.map(|answer| answer.response),
  };
  answer.map(|response| response.status().as_u16())
}

/// The key of a session bound to `bound`, by what the store said. Only a store that was read and
/// doesn't have it means the key is lost: anything else leaves the session alone.
fn key_of_session(
  read: Result<Option<Arc<DpopProver>>, AuthError>,
  bound: &JwkThumbprint,
) -> Result<Arc<DpopProver>, AuthError> {
  match read? {
    Some(prover) if prover.thumbprint() == *bound => Ok(prover),
    _ => Err(AuthError::KeyLost),
  }
}

/// The clock offset an answer names, unless it comes out of a cache on the way.
fn fresh_clock_offset(response: &FetchResponse) -> Option<i64> {
  if is_from_a_cache(response.maybe_age.as_deref()) {
    return None;
  }
  clock_offset_of(response.maybe_date.as_deref()?)
}

/// What an API answer tells the prover.
fn dpop_answer(response: &FetchResponse) -> DpopAnswer<'_> {
  DpopAnswer {
    maybe_nonce: response.maybe_dpop_nonce.as_deref(),
    maybe_date: response
      .maybe_date
      .as_deref()
      .filter(|_| !is_from_a_cache(response.maybe_age.as_deref())),
  }
}

async fn token_attempt(
  method: &str,
  url: &str,
  headers: &[(String, String)],
  body: &[u8],
  maybe_proof: Option<String>,
) -> Result<TokenEndpointAttempt<oauth2::HttpResponse>, HttpError> {
  let mut headers = headers.to_vec();
  if let Some(proof) = maybe_proof {
    headers.push((DPOP_HEADER.to_owned(), proof));
  }
  let answer = fetch(method, url, &headers, Some(body.to_vec()))
    .await
    .map_err(HttpError)?;
  let response = oauth2::http::Response::builder()
    .status(answer.status)
    .body(answer.body.clone())
    .map_err(|error| HttpError(error.to_string()))?;
  Ok(TokenEndpointAttempt {
    status: answer.status,
    body: answer.body,
    maybe_nonce: answer.maybe_dpop_nonce,
    maybe_date: answer.maybe_date,
    response,
  })
}

/// The whole URL of a request: the page's own server is addressed by path, and a proof names it all.
fn absolute_url(url: &str) -> String {
  if !url.starts_with('/') {
    return url.to_owned();
  }
  let origin = web_sys::window()
    .and_then(|window| window.location().origin().ok())
    .unwrap_or_default();
  format!("{origin}{url}")
}

/// `Authorization`, and `DPoP` for a bound token.
async fn sign_headers(
  maybe_prover: Option<&DpopProver>,
  token: &str,
  method: &str,
  url: &str,
) -> Result<Vec<(String, String)>, AuthError> {
  let signed = signed_headers(maybe_prover, token, method, url)
    .await
    .map_err(|error| match error {
      DpopSendError::KeyLost => AuthError::KeyLost,
      DpopSendError::KeyUnavailable => AuthError::Network(error.to_string()),
      DpopSendError::Proof(error) => AuthError::Unsupported(error.to_string()),
    })?;
  let mut headers = vec![("Authorization".to_owned(), signed.authorization)];
  if let Some(proof) = signed.maybe_proof {
    headers.push((DPOP_HEADER.to_owned(), proof.as_str().to_owned()));
  }
  Ok(headers)
}

/// This browser's DPoP key, if it has one.
#[cfg(target_arch = "wasm32")]
async fn stored_prover() -> Result<Option<Arc<DpopProver>>, String> {
  teamcraft_core::store::browser_dpop_prover()
    .await
    .map_err(|error| format!("the device key can't be read: {error}"))
}

/// A new DPoP key for this browser. If that fails the session goes without.
#[cfg(target_arch = "wasm32")]
async fn new_prover() -> Option<Arc<DpopProver>> {
  teamcraft_core::store::browser_dpop_prover_or_new()
    .await
    .inspect_err(|error| tracing::warn!(%error, "could not make a DPoP key"))
    .ok()
}

#[cfg(not(target_arch = "wasm32"))]
async fn stored_prover() -> Result<Option<Arc<DpopProver>>, String> {
  Ok(None)
}

#[cfg(not(target_arch = "wasm32"))]
async fn new_prover() -> Option<Arc<DpopProver>> {
  None
}

type TokenError =
  RequestTokenError<HttpError, oauth2::StandardErrorResponse<oauth2::basic::BasicErrorResponseType>>;

/// The provider refused the refresh token itself (`invalid_grant`): the session is over.
fn is_session_over(error: &TokenError) -> bool {
  matches!(
    error,
    RequestTokenError::ServerResponse(response)
      if *response.error() == oauth2::basic::BasicErrorResponseType::InvalidGrant
  )
}

/// A failed refresh that isn't `invalid_grant` (handled before this): the provider said something
/// (`temporarily_unavailable`, `server_error`, a misconfigured client) that says nothing about the
/// session, which stays for the next try (issue #667). Not a refusal, so nobody is signed out.
fn map_refresh_error(error: TokenError) -> AuthError {
  match map_token_error(error) {
    AuthError::Denied(description) => AuthError::InvalidResponse(description),
    other => other,
  }
}

/// A 401 of the server itself, with its error body. A proxy's or captive portal's page with that
/// status says nothing about the token (robustness.md, "Hostile networks"), and neither do the
/// DPoP codes: a refused proof isn't a refused token, and a new token wouldn't be bound either.
fn is_refused_token(status: u16, body: &[u8]) -> bool {
  status == UNAUTHORIZED
    && serde_json::from_slice::<ErrorBody>(body).is_ok_and(|body| {
      !matches!(
        body.error_code,
        ErrorCode::UseDpopNonce | ErrorCode::InvalidDpopProof | ErrorCode::DpopRequired
      )
    })
}

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
/// Whether tokens saved for the provider `maybe_saved_for` must stay away from this one. Tokens with
/// no such note came from a sign-in of this page, through this provider.
fn is_other_provider(maybe_saved_for: Option<&(String, String)>, issuer: &str, client_id: &str) -> bool {
  maybe_saved_for
    .is_some_and(|(saved_issuer, saved_client)| saved_issuer != issuer || saved_client != client_id)
}

/// The local account's session this tab holds, as long as it lasts.
fn stored_local_session() -> Option<LocalSession> {
  let stored = session_get(LOCAL_SESSION_KEY)?;
  serde_json::from_str::<LocalSession>(&stored)
    .ok()
    .filter(|session| session.expires_at_ms > now_ms())
}

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

/// Removes the OIDC response parameters from the address bar, and goes back to `maybe_path` (a
/// checked route path) if sign-in began somewhere else.
fn clean_location_to(maybe_path: Option<&str>) {
  if let Some(window) = web_sys::window()
    && let Ok(history) = window.history()
  {
    let path = match maybe_path {
      Some(path) => path.to_owned(),
      None => window.location().pathname().unwrap_or_else(|_| "/".to_owned()),
    };
    if history
      .replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&path))
      .is_err()
    {
      tracing::debug!("could not clean the address bar");
    }
  }
}

/// The page's path if it is one of the app's routes other than the start page.
fn current_route_path() -> Option<String> {
  let path = web_sys::window()?.location().pathname().ok()?;
  is_return_path(&path).then_some(path)
}

/// Only paths the app itself would show come back after sign-in: a route, never another origin
/// (`//host`) or anything with a query or fragment.
fn is_return_path(path: &str) -> bool {
  Route::parse(path).is_some_and(|route| route != Route::Start && route.path() == path)
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

/// The extra parameters of a sign-in: one the user started (`maybe_look`) asks for the login page
/// if the server says so (its fresh answer on the sign-in screen first); a silent renewal after a
/// reload never does, but still prefills the account and the language should the provider ask.
fn login_request<'a>(
  sign_in: &SignIn,
  maybe_look: Option<&'a SignInLook>,
  maybe_hint: Option<&'a str>,
  maybe_page_language: Option<&'a str>,
) -> LoginRequest<'a> {
  let is_login_page_shown = maybe_look.is_some_and(|look| {
    look
      .maybe_login_page_always_shown
      .unwrap_or(sign_in.is_login_page_always_shown)
  });
  let maybe_locale = maybe_look
    .and_then(|look| look.maybe_locale.as_deref())
    .or(maybe_page_language);
  LoginRequest {
    is_login_page_shown,
    maybe_login_hint: maybe_hint,
    maybe_locale,
  }
}

/// The app's language as the page declares it (`<html lang>`).
fn document_language() -> Option<String> {
  web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.document_element())
    .and_then(|root| root.get_attribute("lang"))
    .filter(|lang| !lang.is_empty())
}

/// The account last signed in with at `sign_in`'s provider, if this browser knows it.
fn stored_login_hint(sign_in: &SignIn) -> Option<String> {
  let stored: StoredLoginHint = serde_json::from_str(&local_get(LOGIN_HINT_KEY)?).ok()?;
  (stored.issuer == sign_in.issuer && stored.client_id == sign_in.client_id).then_some(stored.hint)
}

fn remember_login_hint(sign_in: &SignIn, hint: String) {
  let stored = StoredLoginHint {
    issuer: sign_in.issuer.clone(),
    client_id: sign_in.client_id.clone(),
    hint,
  };
  if let Ok(json) = serde_json::to_string(&stored) {
    local_set(LOGIN_HINT_KEY, &json);
  }
}

/// Whether this browser has a member signed in (or did before a reload): a meeting link opened
/// then goes to the member's app, not to the guest page.
pub fn is_signed_in_here() -> bool {
  local_get(SIGNED_IN_HINT_KEY).is_some() || session_get(LOCAL_SESSION_KEY).is_some()
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

#[cfg(test)]
mod tests {
  use super::*;

  mod device_key {
    use teamcraft_core::api::dpop::SoftwareDpopSigner;
    use teamcraft_crypto::dpop::DpopSigningKey;

    use super::*;

    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
    fn prover() -> Arc<DpopProver> {
      Arc::new(DpopProver::new(Arc::new(SoftwareDpopSigner::new(
        DpopSigningKey::generate(),
      ))))
    }

    /// An older bundle after a schema bump, IndexedDB refused or busy: the session stays.
    #[test]
    fn a_store_that_cannot_be_read_is_not_a_lost_key() {
      let bound = prover().thumbprint();
      let read = Err(AuthError::Network("the device key can't be read".to_owned()));
      assert!(matches!(key_of_session(read, &bound), Err(AuthError::Network(_))));
    }

    #[test]
    fn only_a_store_without_the_key_is_a_lost_key() {
      let prover = prover();
      let bound = prover.thumbprint();
      assert!(matches!(
        key_of_session(Ok(None), &bound),
        Err(AuthError::KeyLost)
      ));
      let other = self::prover();
      assert!(matches!(
        key_of_session(Ok(Some(other)), &bound),
        Err(AuthError::KeyLost)
      ));
      assert!(key_of_session(Ok(Some(prover)), &bound).is_ok());
    }

    #[test]
    fn a_date_out_of_a_cache_sets_no_clock() {
      let answer = |maybe_age: Option<&str>| FetchResponse {
        status: 200,
        maybe_content_type: None,
        maybe_dpop_nonce: None,
        maybe_date: Some("Sun, 06 Nov 1994 08:49:37 GMT".to_owned()),
        maybe_age: maybe_age.map(str::to_owned),
        maybe_retry_after: None,
        body: Vec::new(),
      };
      assert!(fresh_clock_offset(&answer(None)).is_some());
      assert!(fresh_clock_offset(&answer(Some("0"))).is_some());
      assert_eq!(fresh_clock_offset(&answer(Some("120"))), None);
    }
  }

  mod refresh_errors {
    use oauth2::StandardErrorResponse;
    use oauth2::basic::BasicErrorResponseType;

    use super::*;

    fn provider_says(code: &str) -> TokenError {
      RequestTokenError::ServerResponse(StandardErrorResponse::new(
        BasicErrorResponseType::Extension(code.to_owned()),
        None,
        None,
      ))
    }

    #[test]
    fn only_invalid_grant_is_a_refusal() {
      let refused = RequestTokenError::ServerResponse(StandardErrorResponse::new(
        BasicErrorResponseType::InvalidGrant,
        None,
        None,
      ));
      assert!(is_session_over(&refused));
      for code in ["server_error", "temporarily_unavailable"] {
        let error = provider_says(code);
        assert!(!is_session_over(&error), "{code}");
        assert!(
          matches!(map_refresh_error(error), AuthError::InvalidResponse(_)),
          "{code}"
        );
      }
    }

    /// A refresh token only ever goes to the provider it came from, whatever the server says.
    #[test]
    fn tokens_stay_away_from_another_provider() {
      let saved_for = ("https://id.example.com".to_owned(), "teamcraft".to_owned());
      assert!(!is_other_provider(
        Some(&saved_for),
        "https://id.example.com",
        "teamcraft"
      ));
      assert!(is_other_provider(
        Some(&saved_for),
        "https://evil.example.com",
        "teamcraft"
      ));
      assert!(is_other_provider(
        Some(&saved_for),
        "https://id.example.com",
        "another-client"
      ));
      assert!(!is_other_provider(None, "https://id.example.com", "teamcraft"));
    }

    #[test]
    fn a_401_counts_only_with_the_servers_body() {
      assert!(is_refused_token(
        401,
        br#"{"error_code":"invalid_token","message":"no"}"#
      ));
      assert!(!is_refused_token(
        401,
        b"<html>Proxy authentication required</html>"
      ));
      assert!(!is_refused_token(401, b""));
      assert!(!is_refused_token(
        403,
        br#"{"error_code":"invalid_token","message":"no"}"#
      ));
    }
  }

  #[test]
  fn only_route_paths_come_back_after_sign_in() {
    for path in [
      "/c/123/m/456",
      "/s/1/c/2",
      "/dm/7",
      "/settings/security",
      "/admin/users",
      "/home",
      "/meeting/42",
    ] {
      assert!(is_return_path(path), "{path}");
    }
    for path in [
      "/",
      "",
      "//example.com/c/1",
      "https://example.com/c/1",
      "/c/1?x=1",
      "/c/1#token",
      "/dm/7/",
      "/meeting/42#invite=token",
      "/unknown",
      "c/1",
    ] {
      assert!(!is_return_path(path), "{path:?}");
    }
  }

  /// Issue #448: a sign-in the user starts asks for the login page with the last account and the
  /// app's language; a silent renewal after a reload asks for none of that.
  #[test]
  fn interactive_sign_ins_ask_for_the_login_page_and_silent_ones_do_not() {
    let client = oidc_client(
      ClientId::new("teamcraft".into()),
      AuthUrl::new("https://auth.example.com/authorize".into()).unwrap(),
      TokenUrl::new("https://auth.example.com/token".into()).unwrap(),
      RedirectUrl::new("https://chat.example.com/".into()).unwrap(),
    );
    let scopes = vec![OPENID_SCOPE.to_owned()];
    let interactive = LoginRequest {
      is_login_page_shown: true,
      maybe_login_hint: Some("alice"),
      maybe_locale: Some("en"),
    };
    let (challenge, _) = PkceCodeChallenge::new_random_sha256();
    let (url, _) = authorization_url(&client, &scopes, challenge, &interactive);
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["prompt"], "login");
    assert_eq!(query["login_hint"], "alice");
    assert_eq!(query["ui_locales"], "en");
    let (challenge, _) = PkceCodeChallenge::new_random_sha256();
    let (url, _) = authorization_url(&client, &scopes, challenge, &LoginRequest::default());
    assert!(!url.as_str().contains("prompt=") && !url.as_str().contains("login_hint="));
  }

  /// Issue #448: the user's sign-in asks for the login page (the sign-in screen's fresh answer
  /// first), a silent renewal never does but keeps the hint and the language.
  #[test]
  fn begin_login_asks_for_the_login_page_only_when_the_user_signs_in() {
    let sign_in = SignIn {
      issuer: "https://auth.example.com".into(),
      client_id: "teamcraft".into(),
      scopes: vec![OPENID_SCOPE.to_owned()],
      is_login_page_always_shown: true,
      maybe_clock_offset_secs: None,
      maybe_server_nonce: None,
    };
    let look = SignInLook {
      maybe_locale: Some("de".into()),
      ..SignInLook::default()
    };
    let interactive = login_request(&sign_in, Some(&look), Some("alice"), Some("en"));
    assert!(interactive.is_login_page_shown);
    assert_eq!(interactive.maybe_locale, Some("de"));
    let silent = login_request(&sign_in, None, Some("alice"), Some("en"));
    assert_eq!(
      silent,
      LoginRequest {
        is_login_page_shown: false,
        maybe_login_hint: Some("alice"),
        maybe_locale: Some("en"),
      }
    );
    let turned_off = SignInLook {
      maybe_login_page_always_shown: Some(false),
      ..look
    };
    assert!(!login_request(&sign_in, Some(&turned_off), None, None).is_login_page_shown);
  }

  /// A pending sign-in stored before the return path existed still reads.
  #[test]
  fn pending_without_return_path_reads() {
    let pending: Pending =
      serde_json::from_str(r#"{"state":"s","verifier":"v","redirect_uri":"https://example.com/"}"#).unwrap();
    assert_eq!(pending.maybe_return_path, None);
  }
}
