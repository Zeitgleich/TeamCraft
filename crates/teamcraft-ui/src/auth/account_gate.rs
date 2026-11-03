//! Nothing leaves the app for a cached account until the server confirmed it (issue #778,
//! e2ee.md §9.8): local data opens at once, but the server may name another user for this session
//! (it was reset, the identity provider maps people differently), and what the cached user wrote or
//! queued must not go out under that user's token.
//!
//! This wraps the sign-in of the shell, so every request of the UI and the page's core passes here.
//! `GET /me`, which asks the question, goes through. Other requests wait a moment, since on a working
//! network the answer is about to arrive and the first loads of a start shouldn't fail for nothing.
//! Past that they fail as without a network, and the core gets no token at all.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use teamcraft_core::platform;
use teamcraft_proto::GetMeResponse;

use super::{
  ApiBytes, ApiMethod, ApiResponse, AuthError, Authenticator, BinaryBody, LocalFuture, SignInLook,
};
use crate::clock::now_ms;

/// The one request that goes out before the account is confirmed.
pub const ME_PATH: &str = "/api/v1/me";
/// How long after the start other requests wait for the answer. On a dead network they fail
/// afterwards at once, as with any other request.
const REQUEST_PATIENCE_MS: i64 = 10_000;
const REQUEST_RECHECK: Duration = Duration::from_millis(100);
/// The most rechecks of one request, for a clock that goes backwards meanwhile.
const MAX_RECHECKS: u32 = 100;
const NOT_CONFIRMED: &str = "waiting for the server to confirm the account";

/// [`Authenticator`] of a shell with the rule of this module.
pub struct AccountGate {
  inner: Rc<dyn Authenticator>,
  /// When the start from the cached account began, while its account isn't confirmed.
  maybe_unconfirmed_since_ms: Cell<Option<i64>>,
}

/// What a request does now.
#[derive(Debug, PartialEq, Eq)]
enum GateStep {
  Go,
  Wait,
  Refuse,
}

impl AccountGate {
  pub fn new(inner: Rc<dyn Authenticator>) -> Self {
    Self {
      inner,
      maybe_unconfirmed_since_ms: Cell::new(None),
    }
  }

  async fn wait_for_account(&self, path: &str) -> Result<(), AuthError> {
    for _ in 0..MAX_RECHECKS {
      match gate_step(path, self.maybe_unconfirmed_since_ms.get(), now_ms()) {
        GateStep::Go => return Ok(()),
        GateStep::Refuse => return Err(AuthError::Network(NOT_CONFIRMED.to_owned())),
        GateStep::Wait => platform::sleep(REQUEST_RECHECK).await,
      }
    }
    Err(AuthError::Network(NOT_CONFIRMED.to_owned()))
  }

  fn settled(&self) {
    self.maybe_unconfirmed_since_ms.set(None);
  }
}

fn gate_step(path: &str, maybe_unconfirmed_since_ms: Option<i64>, now_ms: i64) -> GateStep {
  match maybe_unconfirmed_since_ms {
    None => GateStep::Go,
    Some(_) if path == ME_PATH => GateStep::Go,
    Some(since) if now_ms.saturating_sub(since) < REQUEST_PATIENCE_MS => GateStep::Wait,
    Some(_) => GateStep::Refuse,
  }
}

impl Authenticator for AccountGate {
  fn restore(&self) -> LocalFuture<'_, Result<bool, AuthError>> {
    Box::pin(async move {
      let outcome = self.inner.restore().await;
      // The session is over: nothing is left to wait for. A session that is fine still waits for
      // the account.
      if matches!(
        outcome,
        Ok(false) | Err(AuthError::Denied(_) | AuthError::NotSignedIn)
      ) {
        self.settled();
      }
      outcome
    })
  }

  fn cached_account(&self) -> LocalFuture<'_, Option<GetMeResponse>> {
    Box::pin(async move {
      let maybe_account = self.inner.cached_account().await;
      if maybe_account.is_some() {
        // async-ok: a cell of the sign-in, not a signal.
        self.maybe_unconfirmed_since_ms.set(Some(now_ms()));
      }
      maybe_account
    })
  }

  fn remember_account<'a>(&'a self, account: &'a GetMeResponse) -> LocalFuture<'a, ()> {
    self.settled();
    self.inner.remember_account(account)
  }

  fn forget_account(&self) -> LocalFuture<'_, ()> {
    self.settled();
    self.inner.forget_account()
  }

  fn is_account_unconfirmed(&self) -> bool {
    self.maybe_unconfirmed_since_ms.get().is_some()
  }

  fn login(&self, look: SignInLook) -> LocalFuture<'_, Result<(), AuthError>> {
    self.inner.login(look)
  }

  fn cancel_login(&self) {
    self.inner.cancel_login();
  }

  fn logout(&self) -> LocalFuture<'_, Result<(), AuthError>> {
    self.settled();
    self.inner.logout()
  }

  fn api_request(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<String>,
  ) -> LocalFuture<'_, Result<ApiResponse, AuthError>> {
    let path = path.to_owned();
    Box::pin(async move {
      self.wait_for_account(&path).await?;
      self.inner.api_request(method, &path, maybe_body).await
    })
  }

  fn login_local(
    &self,
    login: String,
    password: String,
    maybe_totp: Option<String>,
  ) -> LocalFuture<'_, Result<(), AuthError>> {
    Box::pin(async move {
      let outcome = self.inner.login_local(login, password, maybe_totp).await;
      if outcome.is_ok() {
        self.settled();
      }
      outcome
    })
  }

  /// For the page's core. It never waits: the core tries again by itself.
  fn access_token(&self) -> LocalFuture<'_, Result<String, AuthError>> {
    if self.is_account_unconfirmed() {
      return Box::pin(async { Err(AuthError::Network(NOT_CONFIRMED.to_owned())) });
    }
    self.inner.access_token()
  }

  fn api_request_bytes(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<BinaryBody>,
  ) -> LocalFuture<'_, Result<ApiBytes, AuthError>> {
    let path = path.to_owned();
    Box::pin(async move {
      self.wait_for_account(&path).await?;
      self.inner.api_request_bytes(method, &path, maybe_body).await
    })
  }
}

#[cfg(test)]
mod tests {
  use std::cell::RefCell;

  use futures_util::FutureExt;

  use super::*;

  const OTHER_PATH: &str = "/api/v1/conversations";
  const SINCE_MS: i64 = 1_000_000;

  #[test]
  fn before_the_answer_only_the_question_goes_out() {
    assert_eq!(gate_step(ME_PATH, Some(SINCE_MS), SINCE_MS), GateStep::Go);
    assert_eq!(gate_step(OTHER_PATH, Some(SINCE_MS), SINCE_MS), GateStep::Wait);
    assert_eq!(
      gate_step(OTHER_PATH, Some(SINCE_MS), SINCE_MS + REQUEST_PATIENCE_MS),
      GateStep::Refuse
    );
    assert_eq!(gate_step(OTHER_PATH, None, SINCE_MS), GateStep::Go);
  }

  /// The whole path of a request: refused on the way, without reaching the session's token.
  #[test]
  fn a_request_past_the_patience_never_reaches_the_session() {
    let (gate, inner) = started_gate();
    gate.maybe_unconfirmed_since_ms.set(Some(i64::MIN));
    let outcome = run(gate.api_get(OTHER_PATH));
    assert!(matches!(outcome, Err(AuthError::Network(_))));
    assert_eq!(inner.requests(), Vec::<String>::new());

    let answered = run(gate.api_get(ME_PATH));
    assert!(answered.is_ok());
    assert_eq!(inner.requests(), [ME_PATH]);
  }

  #[test]
  fn the_core_gets_its_token_only_for_a_confirmed_account() {
    let (gate, _) = started_gate();
    assert!(matches!(run(gate.access_token()), Err(AuthError::Network(_))));
    run(gate.remember_account(&account()));
    assert_eq!(run(gate.access_token()).as_deref(), Ok("token"));
  }

  #[test]
  fn a_restored_session_alone_confirms_nothing() {
    let (gate, _) = started_gate();
    assert_eq!(run(gate.restore()), Ok(true));
    assert!(gate.is_account_unconfirmed());
  }

  #[test]
  fn what_settles_the_account_opens_the_gate() {
    let (gate, _) = started_gate();
    run(gate.forget_account());
    assert!(!gate.is_account_unconfirmed());
    let (gate, _) = started_gate();
    drop(run(gate.logout()));
    assert!(!gate.is_account_unconfirmed());
    let (gate, inner) = started_gate();
    *inner.restored.borrow_mut() = Err(AuthError::NotSignedIn);
    drop(run(gate.restore()));
    assert!(!gate.is_account_unconfirmed());
  }

  #[test]
  fn no_answer_keeps_it_closed() {
    let (gate, inner) = started_gate();
    *inner.restored.borrow_mut() = Err(AuthError::Network("down".into()));
    drop(run(gate.restore()));
    assert!(gate.is_account_unconfirmed());
    *inner.restored.borrow_mut() = Err(AuthError::InvalidResponse("server_error".into()));
    drop(run(gate.restore()));
    assert!(gate.is_account_unconfirmed());
  }

  /// A start without a cached account has nothing to wait for.
  #[test]
  fn without_a_cached_account_nothing_waits() {
    let inner = Rc::new(FakeSession::default());
    let gate = AccountGate::new(inner.clone());
    assert!(!gate.is_account_unconfirmed());
    assert!(run(gate.api_get(OTHER_PATH)).is_ok());
    assert_eq!(inner.requests(), [OTHER_PATH]);
  }

  fn started_gate() -> (AccountGate, Rc<FakeSession>) {
    let inner = Rc::new(FakeSession::default());
    *inner.maybe_cached.borrow_mut() = Some(account());
    let gate = AccountGate::new(inner.clone());
    assert!(run(gate.cached_account()).is_some());
    (gate, inner)
  }

  fn run<T>(future: LocalFuture<'_, T>) -> T {
    future.now_or_never().expect("nothing here waits")
  }

  fn account() -> GetMeResponse {
    serde_json::from_str(r#"{"id":"42","display_name":"Alice","role":"owner"}"#).expect("an account")
  }

  /// A session that answers at once and notes the requests that reach it.
  struct FakeSession {
    maybe_cached: RefCell<Option<GetMeResponse>>,
    restored: RefCell<Result<bool, AuthError>>,
    requests: RefCell<Vec<String>>,
  }

  impl Default for FakeSession {
    fn default() -> Self {
      Self {
        maybe_cached: RefCell::new(None),
        restored: RefCell::new(Ok(true)),
        requests: RefCell::new(Vec::new()),
      }
    }
  }

  impl FakeSession {
    fn requests(&self) -> Vec<String> {
      self.requests.borrow().clone()
    }
  }

  impl Authenticator for FakeSession {
    fn restore(&self) -> LocalFuture<'_, Result<bool, AuthError>> {
      Box::pin(async { self.restored.borrow().clone() })
    }

    fn cached_account(&self) -> LocalFuture<'_, Option<GetMeResponse>> {
      Box::pin(async { self.maybe_cached.borrow().clone() })
    }

    fn login(&self, _look: SignInLook) -> LocalFuture<'_, Result<(), AuthError>> {
      Box::pin(async { Ok(()) })
    }

    fn logout(&self) -> LocalFuture<'_, Result<(), AuthError>> {
      Box::pin(async { Ok(()) })
    }

    fn access_token(&self) -> LocalFuture<'_, Result<String, AuthError>> {
      Box::pin(async { Ok("token".to_owned()) })
    }

    fn api_request(
      &self,
      _method: ApiMethod,
      path: &str,
      _maybe_body: Option<String>,
    ) -> LocalFuture<'_, Result<ApiResponse, AuthError>> {
      self.requests.borrow_mut().push(path.to_owned());
      Box::pin(async {
        Ok(ApiResponse {
          status: 200,
          body: String::new(),
        })
      })
    }
  }
}
