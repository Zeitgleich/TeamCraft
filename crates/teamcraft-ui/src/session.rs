//! The signed-in session of the UI: where in the sign-in flow the user is and what the server says
//! about the account. Pure state; the screens live in `components::login`.

use std::rc::Rc;
use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{ErrorBody, ErrorCode, GetMeResponse};

use teamcraft_client_api::remote::{CoreStatus, EnrollRequest};

use crate::auth::tauri::{ServerCheck, ServerProblem, ServerView};
use crate::auth::{ApiResponse, AuthError, Authenticator, tauri};
use crate::clock;
use crate::model::UserId;
use crate::remote_backend;
use crate::settings::{load_string, store_string};

const KEY_DEVICE_READY_PREFIX: &str = "teamcraft.device_ready.";
/// How long the client core may wait for its access token before the user is told; it keeps
/// trying, and the problem card goes away once it opened.
const CORE_START_PATIENCE_MS: i64 = 10_000;
/// How often the UI asks a waiting core again: often at first, calmly once the card is shown.
const CORE_POLL_EARLY: Duration = Duration::from_millis(500);
const CORE_POLL_LATE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq)]
pub enum SessionState {
  /// Looking for a stored session or finishing a redirect.
  Checking,
  /// Desktop: no server is chosen yet (first start, or after "Change server"). The address is put
  /// back into the field after a failed check.
  NeedsServer {
    maybe_address: Option<String>,
    maybe_problem: Option<ServerProblem>,
  },
  /// Desktop: the server answered; the user confirms its name and host before signing in.
  /// Boxed: the brand in it makes it much larger than the other states.
  ConfirmServer(Box<ServerCheck>),
  SignedOut(Option<LoginProblem>),
  /// The identity provider is open in the browser.
  SigningIn,
  /// Desktop: signed in; the client core is still waiting for the access token to open the
  /// encrypted data.
  OpeningCore,
  /// Signed in, but this device has no keys yet (first device of the account); the web build only
  /// shows a placeholder here.
  NeedsDeviceSetup(GetMeResponse),
  /// Desktop: this device has to create its encryption keys or recover them with the passphrase.
  NeedsEnrollment {
    me: GetMeResponse,
    has_existing_identity: bool,
    /// This browser had the account set up, but its data is gone (e2ee.md §9.4).
    was_data_lost: bool,
  },
  Ready(GetMeResponse),
}

/// Why the user is back at the login screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginProblem {
  /// The account signed in but has no TeamCraft role (`no_teamcraft_role`).
  NoTeamcraftRole,
  ServerUnreachable,
  /// The encrypted local data could not be opened (keystore, database); the text is for details.
  CoreFailed(String),
  /// The client core has been waiting for an access token for a while; it keeps trying, and the user
  /// can try again or sign out.
  CoreNotSignedIn,
  Cancelled,
  /// Anything else; the text is meant for logs and a "details" line, not as the main message.
  Failed(String),
}

/// Drives the sign-in flow. Cheap to copy; provided through context.
#[derive(Clone, Copy)]
pub struct SessionControl {
  auth: StoredValue<Rc<dyn Authenticator>, LocalStorage>,
  pub state: RwSignal<SessionState>,
  /// Why the last enrollment attempt failed, for the enrollment screen.
  pub enroll_error: RwSignal<Option<String>>,
  pub is_enrolling: RwSignal<bool>,
  /// Why the last local sign-in failed, for its form.
  pub local_error: RwSignal<Option<LocalSignInProblem>>,
  pub is_local_signing_in: RwSignal<bool>,
  /// Desktop: the account's server, shown on the sign-in screen.
  pub server: RwSignal<Option<ServerView>>,
  /// Desktop: the device's managed configuration does not allow another server.
  pub is_server_locked: RwSignal<bool>,
  /// Desktop: a server address is being checked.
  pub is_checking_server: RwSignal<bool>,
  /// Desktop: counts core starts, so waiting for an older one stops (sign-out, retry).
  core_round: RwSignal<u64>,
}

/// What went wrong in the local-account sign-in form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalSignInProblem {
  /// Wrong e-mail, password or code, or a code is missing (the server does not say which).
  Refused,
  /// Sign-in with e-mail and password is turned off or not set up on this server.
  Disabled,
  /// Too many attempts; try again later.
  RateLimited,
  /// The server could not be reached.
  Network,
  /// Anything else; the text is for a details line.
  Failed(String),
}

impl SessionControl {
  pub fn new(auth: Rc<dyn Authenticator>) -> Self {
    Self {
      auth: StoredValue::new_local(auth),
      state: RwSignal::new(SessionState::Checking),
      enroll_error: RwSignal::new(None),
      is_enrolling: RwSignal::new(false),
      local_error: RwSignal::new(None),
      is_local_signing_in: RwSignal::new(false),
      server: RwSignal::new(None),
      is_server_locked: RwSignal::new(false),
      is_checking_server: RwSignal::new(false),
      core_round: RwSignal::new(0),
    }
  }

  pub fn authenticator(&self) -> Rc<dyn Authenticator> {
    self.auth.get_value()
  }

  /// Looks for an existing session at startup. The desktop app first needs a server.
  pub fn start(&self) {
    let control = *self;
    spawn_local(async move {
      if tauri::is_available() && !control.has_server().await {
        return;
      }
      let auth = control.authenticator();
      match auth.restore().await {
        Ok(true) => control.load_account().await,
        Ok(false) => control.state.set(SessionState::SignedOut(None)),
        Err(error) => control
          .state
          .set(SessionState::SignedOut(Some(problem_from_auth(&error)))),
      }
    });
  }

  /// Checks the server the user typed (desktop); its answer is shown for confirmation.
  pub fn check_server(&self, address: String) {
    let control = *self;
    control.is_checking_server.set(true);
    spawn_local(async move {
      let outcome = tauri::server_check(&address).await;
      control.is_checking_server.set(false);
      control.state.set(match outcome {
        Ok(check) => SessionState::ConfirmServer(Box::new(check)),
        Err(problem) => SessionState::NeedsServer {
          maybe_address: Some(address),
          maybe_problem: Some(problem),
        },
      });
    });
  }

  /// The user confirmed the checked server: it becomes the account's server and sign-in starts.
  pub fn confirm_server(&self, check: ServerCheck) {
    let control = *self;
    control.is_checking_server.set(true);
    spawn_local(async move {
      let outcome = tauri::server_confirm(&check.server.origin).await;
      control.is_checking_server.set(false);
      match outcome {
        Ok(server) => {
          crate::brand::show_server_brand(check.brand);
          control.server.set(Some(server));
          control.sign_in();
        },
        Err(problem) => control.state.set(SessionState::NeedsServer {
          maybe_address: Some(check.server.host),
          maybe_problem: Some(problem),
        }),
      }
    });
  }

  /// Back to the server entry (desktop), from the confirmation or the sign-in screen.
  pub fn change_server(&self) {
    let control = *self;
    spawn_local(async move {
      let had_server = control.server.get_untracked().is_some();
      if had_server && let Err(problem) = tauri::server_forget().await {
        control.state.set(SessionState::NeedsServer {
          maybe_address: None,
          maybe_problem: Some(problem),
        });
        return;
      }
      control.server.set(None);
      control.state.set(SessionState::NeedsServer {
        maybe_address: None,
        maybe_problem: None,
      });
    });
  }

  /// Desktop: whether the shell has an account server. Without one this offers the managed
  /// server (checked right away) or the empty server entry, and answers `false`.
  async fn has_server(&self) -> bool {
    let server_state = match tauri::server_state().await {
      Ok(server_state) => server_state,
      Err(problem) => {
        self.state.set(SessionState::NeedsServer {
          maybe_address: None,
          maybe_problem: Some(problem),
        });
        return false;
      },
    };
    self.is_server_locked.set(server_state.is_locked);
    if let Some(server) = server_state.maybe_active {
      self.server.set(Some(server));
      return true;
    }
    match server_state.maybe_managed_address {
      Some(address) => self.check_server(address),
      None => self.state.set(SessionState::NeedsServer {
        maybe_address: None,
        maybe_problem: None,
      }),
    }
    false
  }

  pub fn sign_in(&self) {
    let control = *self;
    control.state.set(SessionState::SigningIn);
    spawn_local(async move {
      match control.authenticator().login().await {
        Ok(()) => control.load_account().await,
        Err(error) => control
          .state
          .set(SessionState::SignedOut(Some(problem_from_auth(&error)))),
      }
    });
  }

  /// Signs in a local account (e-mail and password instead of the identity provider).
  pub fn sign_in_local(&self, login: String, password: String, maybe_totp: Option<String>) {
    let control = *self;
    control.local_error.set(None);
    control.is_local_signing_in.set(true);
    spawn_local(async move {
      let outcome = control
        .authenticator()
        .login_local(login, password, maybe_totp)
        .await;
      control.is_local_signing_in.set(false);
      match outcome {
        Ok(()) => control.load_account().await,
        Err(error) => control.local_error.set(Some(local_problem(&error))),
      }
    });
  }

  /// Stops waiting for the system browser (desktop). The pending sign-in then ends as cancelled.
  pub fn cancel_sign_in(&self) {
    self.authenticator().cancel_login();
  }

  pub fn sign_out(&self) {
    let control = *self;
    control.core_round.update(|round| *round = round.wrapping_add(1));
    spawn_local(async move {
      if tauri::has_core()
        && let Err(error) = remote_backend::stop_core().await
      {
        tracing::warn!(%error, "could not stop the client core");
      }
      if let Err(error) = control.authenticator().logout().await {
        tracing::warn!(%error, "sign-out did not finish cleanly");
      }
      control.state.set(SessionState::SignedOut(None));
    });
  }

  /// First-device setup is done (or skipped in the demo): remember it and enter the app.
  pub fn finish_device_setup(&self) {
    if let SessionState::NeedsDeviceSetup(me) = self.state.get_untracked() {
      store_string(&device_ready_key(&me), "1");
      self.state.set(SessionState::Ready(me));
    }
  }

  /// Desktop: opens the encrypted local client and decides whether keys must be set up first.
  async fn start_core(&self, me: GetMeResponse) {
    let round = self.core_round.get_untracked().wrapping_add(1);
    self.core_round.set(round);
    self.check_core(me, clock::now_ms(), round).await;
  }

  async fn check_core(&self, me: GetMeResponse, started_ms: i64, round: u64) {
    if self.core_round.get_untracked() != round {
      return;
    }
    let outcome = remote_backend::core_status(me.id).await;
    // Signed out or started again meanwhile: this answer is stale.
    if self.core_round.get_untracked() != round {
      return;
    }
    match outcome {
      Ok(CoreStatus::Ready { .. }) => self.state.set(SessionState::Ready(me)),
      Ok(CoreStatus::NeedsEnrollment {
        has_existing_identity,
        was_data_lost,
      }) => {
        self.state.set(SessionState::NeedsEnrollment {
          me,
          has_existing_identity,
          was_data_lost,
        });
      },
      Ok(CoreStatus::Failed { message }) => self
        .state
        .set(SessionState::SignedOut(Some(LoginProblem::CoreFailed(message)))),
      // The core is still waiting for an access token and retries on its own.
      Ok(CoreStatus::NotStarted) => self.wait_for_core(me, started_ms, round),
      Err(error) => self
        .state
        .set(SessionState::SignedOut(Some(LoginProblem::Failed(error)))),
    }
  }

  /// Shows "connecting" (after a while the problem card) and asks the core again shortly.
  fn wait_for_core(&self, me: GetMeResponse, started_ms: i64, round: u64) {
    let waited_ms = clock::now_ms().saturating_sub(started_ms);
    let next = core_wait_state(waited_ms);
    if self.state.get_untracked() != next {
      self.state.set(next);
    }
    let poll = if waited_ms < CORE_START_PATIENCE_MS {
      CORE_POLL_EARLY
    } else {
      CORE_POLL_LATE
    };
    let control = *self;
    set_timeout(
      move || spawn_local(async move { control.check_core(me, started_ms, round).await }),
      poll,
    );
  }

  /// The core could not be opened: asks the server and the core again, keeping the sign-in.
  pub fn retry_core(&self) {
    let control = *self;
    control.state.set(SessionState::SigningIn);
    spawn_local(async move { control.load_account().await });
  }

  /// Creates this device's keys (or recovers them) with the passphrase, then enters the app.
  pub fn enroll(&self, passphrase: String, device_name: String) {
    let control = *self;
    let SessionState::NeedsEnrollment { me, .. } = control.state.get_untracked() else {
      return;
    };
    control.is_enrolling.set(true);
    control.enroll_error.set(None);
    spawn_local(async move {
      let request = EnrollRequest {
        passphrase,
        device_name,
      };
      match remote_backend::enroll(&request).await {
        Ok(()) => control.state.set(SessionState::Ready(me)),
        Err(error) => control.enroll_error.set(Some(error)),
      }
      control.is_enrolling.set(false);
    });
  }

  /// The account the enrollment screen is for.
  pub fn enrolling_user(&self) -> Option<UserId> {
    match self.state.get_untracked() {
      SessionState::NeedsEnrollment { me, .. } => Some(me.id),
      _ => None,
    }
  }

  /// The core received the keys of another device: enter the app.
  pub fn finish_link(&self) {
    if let SessionState::NeedsEnrollment { me, .. } = self.state.get_untracked() {
      self.state.set(SessionState::Ready(me));
    }
  }

  async fn load_account(&self) {
    let response = self.authenticator().api_get("/api/v1/me").await;
    let outcome = response
      .map_err(|error| problem_from_auth(&error))
      .and_then(interpret_me_response);
    match outcome {
      // The desktop shell hosts the client core, whose enrollment replaces the demo welcome step.
      Ok(me) if tauri::has_core() => self.start_core(me).await,
      Ok(me) => {
        let is_ready = load_string(&device_ready_key(&me)).is_some();
        self.state.set(if is_ready {
          SessionState::Ready(me)
        } else {
          SessionState::NeedsDeviceSetup(me)
        });
      },
      Err(problem) => {
        // A role-less account stays signed in at the identity provider; the login screen offers to sign out.
        self.state.set(SessionState::SignedOut(Some(problem)));
      },
    }
  }
}

pub fn use_session() -> Option<SessionControl> {
  use_context::<SessionControl>()
}

/// While the core waits for its access token: "connecting" at first, the problem card once the
/// wait is longer than [`CORE_START_PATIENCE_MS`].
fn core_wait_state(waited_ms: i64) -> SessionState {
  if waited_ms < CORE_START_PATIENCE_MS {
    SessionState::OpeningCore
  } else {
    SessionState::SignedOut(Some(LoginProblem::CoreNotSignedIn))
  }
}

fn device_ready_key(me: &GetMeResponse) -> String {
  format!("{KEY_DEVICE_READY_PREFIX}{}", me.id)
}

/// The browser build gets the server's `error_code` (or `http_{status}`), the desktop shell its own
/// kinds; both end up as one of the form's messages. Wrong e-mail, password and code share one.
fn local_problem(error: &AuthError) -> LocalSignInProblem {
  match error {
    AuthError::Denied(code) => match code.as_str() {
      "unauthorized" | "refused" | "mfa_required" | "invalid_request" | "http_400" | "http_401" => {
        LocalSignInProblem::Refused
      },
      "local_accounts_disabled" | "disabled" | "unavailable" | "http_404" | "http_503" => {
        LocalSignInProblem::Disabled
      },
      "rate_limited" | "http_429" => LocalSignInProblem::RateLimited,
      other => LocalSignInProblem::Failed(other.to_owned()),
    },
    AuthError::Network(_) => LocalSignInProblem::Network,
    other => LocalSignInProblem::Failed(other.to_string()),
  }
}

fn problem_from_auth(error: &AuthError) -> LoginProblem {
  match error {
    AuthError::Cancelled => LoginProblem::Cancelled,
    AuthError::Network(_) => LoginProblem::ServerUnreachable,
    other => LoginProblem::Failed(other.to_string()),
  }
}

/// Turns the answer of `GET /api/v1/me` into the account or the reason it cannot be used.
pub fn interpret_me_response(response: ApiResponse) -> Result<GetMeResponse, LoginProblem> {
  match response.status {
    200 => serde_json::from_str(&response.body)
      .map_err(|error| LoginProblem::Failed(format!("unreadable account response: {error}"))),
    status => match serde_json::from_str::<ErrorBody>(&response.body) {
      Ok(body) if body.error_code == ErrorCode::NoTeamcraftRole => Err(LoginProblem::NoTeamcraftRole),
      Ok(body) => Err(LoginProblem::Failed(format!("{} ({status})", body.error_code))),
      Err(_) => Err(LoginProblem::Failed(format!("HTTP {status}"))),
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const ME: &str = r#"{"id":"42","username":"alice","display_name":"Alice","role":"owner"}"#;

  #[test]
  fn account_is_parsed() {
    let me = interpret_me_response(ApiResponse {
      status: 200,
      body: ME.into(),
    })
    .expect("valid account");
    assert_eq!(me.display_name, "Alice");
  }

  #[test]
  fn missing_role_gets_its_own_problem() {
    let body = r#"{"error_code":"no_teamcraft_role","message":"no role"}"#;
    let outcome = interpret_me_response(ApiResponse {
      status: 403,
      body: body.into(),
    });
    assert_eq!(outcome, Err(LoginProblem::NoTeamcraftRole));
  }

  #[test]
  fn other_errors_keep_the_code() {
    let body = r#"{"error_code":"invalid_token","message":"expired"}"#;
    let outcome = interpret_me_response(ApiResponse {
      status: 401,
      body: body.into(),
    });
    assert_eq!(outcome, Err(LoginProblem::Failed("invalid_token (401)".into())));
    assert!(matches!(
      interpret_me_response(ApiResponse { status: 502, body: "<html>".into() }),
      Err(LoginProblem::Failed(text)) if text == "HTTP 502"
    ));
  }

  #[test]
  fn local_sign_in_problems_map_server_codes_and_shell_kinds() {
    let cases = [
      ("unauthorized", LocalSignInProblem::Refused),
      ("refused", LocalSignInProblem::Refused),
      ("mfa_required", LocalSignInProblem::Refused),
      ("invalid_request", LocalSignInProblem::Refused),
      ("local_accounts_disabled", LocalSignInProblem::Disabled),
      ("disabled", LocalSignInProblem::Disabled),
      ("http_503", LocalSignInProblem::Disabled),
      ("rate_limited", LocalSignInProblem::RateLimited),
      ("http_429", LocalSignInProblem::RateLimited),
      ("internal", LocalSignInProblem::Failed("internal".into())),
    ];
    for (code, expected) in cases {
      assert_eq!(local_problem(&AuthError::Denied(code.into())), expected, "{code}");
    }
    assert_eq!(
      local_problem(&AuthError::Network("down".into())),
      LocalSignInProblem::Network
    );
    assert!(matches!(
      local_problem(&AuthError::InvalidResponse("x".into())),
      LocalSignInProblem::Failed(_)
    ));
  }

  #[test]
  fn auth_errors_map_to_problems() {
    assert_eq!(
      problem_from_auth(&AuthError::Network("down".into())),
      LoginProblem::ServerUnreachable
    );
    assert_eq!(problem_from_auth(&AuthError::Cancelled), LoginProblem::Cancelled);
  }

  #[test]
  fn a_core_waiting_for_its_token_shows_connecting_before_the_problem_card() {
    assert_eq!(core_wait_state(0), SessionState::OpeningCore);
    assert_eq!(
      core_wait_state(CORE_START_PATIENCE_MS - 1),
      SessionState::OpeningCore
    );
    assert_eq!(
      core_wait_state(CORE_START_PATIENCE_MS),
      SessionState::SignedOut(Some(LoginProblem::CoreNotSignedIn))
    );
  }
}
