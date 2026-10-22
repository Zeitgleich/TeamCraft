//! The signed-in session of the UI: where in the sign-in flow the user is and what the server says
//! about the account. Pure state; the screens live in `components::login`.

use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{ErrorBody, ErrorCode, GetMeResponse};

use teamcraft_client_api::remote::{CoreStatus, EnrollRequest};

use crate::auth::{ApiResponse, AuthError, Authenticator, tauri};
use crate::model::UserId;
use crate::remote_backend;
use crate::settings::{load_string, store_string};

const KEY_DEVICE_READY_PREFIX: &str = "teamcraft.device_ready.";

#[derive(Clone, Debug, PartialEq)]
pub enum SessionState {
  /// Looking for a stored session or finishing a redirect.
  Checking,
  SignedOut(Option<LoginProblem>),
  /// The identity provider is open in the browser.
  SigningIn,
  /// Signed in, but this device has no keys yet (first device of the account); the web build only
  /// shows a placeholder here.
  NeedsDeviceSetup(GetMeResponse),
  /// Desktop: this device has to create its encryption keys or recover them with the passphrase.
  NeedsEnrollment {
    me: GetMeResponse,
    has_existing_identity: bool,
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
}

impl SessionControl {
  pub fn new(auth: Rc<dyn Authenticator>) -> Self {
    Self {
      auth: StoredValue::new_local(auth),
      state: RwSignal::new(SessionState::Checking),
      enroll_error: RwSignal::new(None),
      is_enrolling: RwSignal::new(false),
    }
  }

  pub fn authenticator(&self) -> Rc<dyn Authenticator> {
    self.auth.get_value()
  }

  /// Looks for an existing session at startup.
  pub fn start(&self) {
    let control = *self;
    spawn_local(async move {
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

  /// Stops waiting for the system browser (desktop). The pending sign-in then ends as cancelled.
  pub fn cancel_sign_in(&self) {
    self.authenticator().cancel_login();
  }

  pub fn sign_out(&self) {
    let control = *self;
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
    match remote_backend::core_status(me.id).await {
      Ok(CoreStatus::Ready { .. }) => self.state.set(SessionState::Ready(me)),
      Ok(CoreStatus::NeedsEnrollment {
        has_existing_identity,
      }) => {
        self.state.set(SessionState::NeedsEnrollment {
          me,
          has_existing_identity,
        });
      },
      Ok(CoreStatus::Failed { message }) => self
        .state
        .set(SessionState::SignedOut(Some(LoginProblem::CoreFailed(message)))),
      Ok(CoreStatus::NotStarted) => {
        self
          .state
          .set(SessionState::SignedOut(Some(LoginProblem::CoreFailed(
            "the client core did not start".to_owned(),
          ))));
      },
      Err(error) => self
        .state
        .set(SessionState::SignedOut(Some(LoginProblem::Failed(error)))),
    }
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

fn device_ready_key(me: &GetMeResponse) -> String {
  format!("{KEY_DEVICE_READY_PREFIX}{}", me.id)
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
  fn auth_errors_map_to_problems() {
    assert_eq!(
      problem_from_auth(&AuthError::Network("down".into())),
      LoginProblem::ServerUnreachable
    );
    assert_eq!(problem_from_auth(&AuthError::Cancelled), LoginProblem::Cancelled);
  }
}
