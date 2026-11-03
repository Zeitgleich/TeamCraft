//! The signed-in session of the UI: where in the sign-in flow the user is and what the server says
//! about the account. Pure state; the screens live in `components::login`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use futures_channel::oneshot;
use futures_util::future::{Either, select};
use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_core::platform;
use teamcraft_proto::{ErrorBody, ErrorCode, GetMeResponse};

use teamcraft_client_api::oidc_login::AuthorizationErrorKind;
use teamcraft_client_api::remote::{CoreStatus, EnrollRequest, EnrollSecret, StoreProblem};

use crate::auth::tauri::{ServerCheck, ServerProblem, ServerView};
use crate::auth::{ApiResponse, AuthError, Authenticator, ME_PATH, SignInLook, tauri};
use crate::clock;
use crate::model::UserId;
use crate::reactive::SetIfAlive;
use crate::remote_backend;
use crate::settings::{load_string, store_string};

const KEY_DEVICE_READY_PREFIX: &str = "teamcraft.device_ready.";
/// How long the client core may wait for its access token before the user is told; it keeps
/// trying, and the problem card goes away once it opened.
const CORE_START_PATIENCE_MS: i64 = 10_000;
/// How often the UI asks a waiting core again: often at first, calmly once the card is shown.
const CORE_POLL_EARLY: Duration = Duration::from_millis(500);
const CORE_POLL_LATE: Duration = Duration::from_secs(2);
/// How long the check of a cached account waits after getting no answer, doubling up to the maximum,
/// plus a part of a second so tabs and devices don't ask together. The network coming back ends the
/// wait ([`SessionControl::check_account_now`]); this is what's left for a network that came back
/// without anyone saying so, and nothing connects before the answer, so it stays short.
const ACCOUNT_CHECK_RETRY_MS: u64 = 2_000;
const ACCOUNT_CHECK_RETRY_MAX_MS: u64 = 30_000;
/// How long the check waits for one answer before it asks again, doubling up to the maximum, so a
/// request that hangs (a dead link the browser hasn't given up on) doesn't hold the app back, and a
/// link slower than this still gets an answer in the end. The request that was given up on runs on:
/// a token refresh in it must finish and save what it got.
const ACCOUNT_ANSWER_PATIENCE_MS: u64 = 10_000;
const ACCOUNT_ANSWER_PATIENCE_MAX_MS: u64 = 60_000;

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
  /// This device has to create its encryption keys or recover them with the recovery kit (or a
  /// legacy passphrase).
  NeedsEnrollment {
    me: GetMeResponse,
    has_existing_identity: bool,
    /// This browser had the account set up, but its data is gone (e2ee.md §9.4).
    was_data_lost: bool,
    /// Desktop: the OS keychain kept the account's backup key (e2ee.md §6.5.6).
    can_restore_on_device: bool,
  },
  Ready(GetMeResponse),
}

impl SessionState {
  /// The signed-in account, in the states that know it.
  pub fn maybe_user(&self) -> Option<crate::model::UserId> {
    match self {
      Self::NeedsDeviceSetup(me) | Self::NeedsEnrollment { me, .. } | Self::Ready(me) => Some(me.id),
      _ => None,
    }
  }
}

/// Why the user is back at the login screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginProblem {
  /// The account signed in but has no TeamCraft role (`no_teamcraft_role`).
  NoTeamcraftRole,
  /// The account is deactivated (`account_deactivated`); an admin has to reactivate it.
  AccountDeactivated,
  ServerUnreachable,
  /// The encrypted local data could not be opened (keystore, database); the text is for details.
  CoreFailed(String),
  /// The local database can't be read by this version (issue #160): set the device up again, or
  /// update the app.
  StoreUnreadable(StoreProblem),
  /// The client core stopped on an internal error (not the keystore or the database): restart the
  /// app and report it.
  CoreStopped,
  /// The client core has been waiting for an access token for a while; it keeps trying, and the user
  /// can try again or sign out.
  CoreNotSignedIn,
  /// The identity provider no longer accepts the stored sign-in (`invalid_grant`: its idle or
  /// session limit ran out, it was revoked): sign in again (issue #883).
  SessionExpired,
  /// The session was ended before its token ran out (signed out at the identity provider or on
  /// another device; `session_ended`): sign in again.
  SessionEnded,
  /// The workspace's idle limit ended the session (`session_timed_out`): sign in again.
  SessionTimedOut,
  /// The account no longer exists (`account_deleted`).
  AccountDeleted,
  /// The identity provider refused the sign-in in its redirect (issue #1063): `kind` picks the
  /// message, `code` is the provider's `error`, shown as the detail.
  ProviderRefused {
    kind: AuthorizationErrorKind,
    code: String,
  },
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
  /// Desktop: the server being checked came from a link, not from the user (docs/app-links.md).
  pub is_server_from_link: RwSignal<bool>,
  /// Counts core starts, so waiting for an older one stops (sign-out, retry). A check of the cached
  /// account belongs to the start it began with.
  core_round: RwSignal<u64>,
  /// The app runs from the cached account (issue #778) and the server hasn't said yet whether it
  /// stands: what the user sees may be out of date, and the connection hint says so sooner.
  pub is_account_unconfirmed: RwSignal<bool>,
  /// What the server said about the account while the core was still opening.
  maybe_confirmed: RwSignal<Option<GetMeResponse>>,
  /// Ends the wait of the running check of the cached account.
  maybe_check_wake: StoredValue<Rc<RefCell<Option<oneshot::Sender<()>>>>, LocalStorage>,
}

/// What restoring the session means for an app that already runs from the cached account.
#[derive(Debug, PartialEq)]
enum RestoreVerdict {
  SignedIn,
  /// No answer, or one that makes no sense (a captive portal): the session stays, ask again.
  NoVerdict,
  /// The session is over; the sign-in screen says why when there is a reason.
  Over(Option<LoginProblem>),
}

/// What the server's account answer means for an app that already runs from the cached account.
#[derive(Debug, PartialEq)]
enum AccountVerdict {
  Confirmed(GetMeResponse),
  NoVerdict,
  /// The token was refused without a reason a new one couldn't fix (expired after a suspend, a wrong
  /// clock): restore the session and ask once more.
  RestoreFirst,
  /// The server refuses this account or session.
  Refused(LoginProblem),
  /// The identity provider refused the refresh token while asking.
  SessionOver,
}

/// Whether the network coming back ends a wait for an answer.
#[derive(Clone, Copy)]
enum Wake {
  Cuts,
  Ignored,
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
  /// The account is deactivated; an admin has to reactivate it.
  Deactivated,
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
      is_server_from_link: RwSignal::new(false),
      core_round: RwSignal::new(0),
      is_account_unconfirmed: RwSignal::new(false),
      maybe_confirmed: RwSignal::new(None),
      maybe_check_wake: StoredValue::new_local(Rc::default()),
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
      match control.authenticator().cached_account().await {
        Some(cached) if tauri::has_core() => control.start_from_cached_account(cached).await,
        _ => control.restore_and_load().await,
      }
    });
  }

  /// A start without a cached account, or one the local data couldn't carry: the session first, then
  /// the account, then the core.
  async fn restore_and_load(&self) {
    match self.authenticator().restore().await {
      Ok(true) => self.load_account().await,
      Ok(false) => {
        self.state.set_if_alive(SessionState::SignedOut(None));
        // A sign-out in a tab closed before it finished may have left a cache behind (#749).
        crate::persisted_media::sweep_signed_out().await;
      },
      // The provider refused the stored sign-in: say so instead of a bare sign-in screen.
      Err(AuthError::Denied(_)) => self
        .state
        .set_if_alive(SessionState::SignedOut(Some(LoginProblem::SessionExpired))),
      // It refused in its redirect: the end of a refused session's round trip, or of a sign-in.
      // Either way nobody is signed in, so no pictures stay (#749).
      Err(error @ AuthError::AuthorizationRefused(_)) => {
        tracing::warn!(%error, "the sign-in was refused");
        self
          .state
          .set_if_alive(SessionState::SignedOut(Some(problem_from_auth(&error))));
        crate::persisted_media::sweep_signed_out().await;
      },
      Err(error) => {
        tracing::warn!(%error, "the stored sign-in could not be restored");
        self
          .state
          .set_if_alive(SessionState::SignedOut(Some(problem_from_auth(&error))));
      },
    }
  }

  /// Opens the local data of the cached account at once and checks the account beside it (issue
  /// #778). The account only picks whose local data opens, on a device it was set up on; nothing
  /// goes out until the server confirmed it. When the local data can't carry the start (not set up here any
  /// more, unreadable), the check stops and the start goes the long way, which knows what to do.
  async fn start_from_cached_account(&self, cached: GetMeResponse) {
    let round = self.next_core_round();
    self.is_account_unconfirmed.set(true);
    let control = *self;
    let checked = cached.clone();
    spawn_local(async move { control.check_cached_account(checked, round).await });
    let status = remote_backend::core_status(cached.id).await;
    if self.core_round.try_get_untracked() != Some(round) {
      return;
    }
    match status {
      Ok(CoreStatus::Ready { .. }) => {
        let me = self
          .maybe_confirmed
          .try_get_untracked()
          .flatten()
          .unwrap_or(cached);
        self.state.set_if_alive(SessionState::Ready(me));
      },
      other => {
        tracing::info!(
          ?other,
          "the local data can't carry the start: asking the server first"
        );
        self.next_core_round();
        self.is_account_unconfirmed.set_if_alive(false);
        self.authenticator().forget_account().await;
        self.restore_and_load().await;
      },
    }
  }

  /// Asks the identity provider and the server about the cached account until one of them answers.
  /// No answer changes nothing: nobody is signed out because the network is gone.
  async fn check_cached_account(&self, cached: GetMeResponse, round: u64) {
    let auth = self.authenticator();
    let mut needs_restore = true;
    let mut has_restored_for_refusal = false;
    let mut attempt = 0_u32;
    loop {
      if needs_restore {
        let restoring = auth.clone();
        // Not cut short by the network coming back: the first restore may be about to go to the
        // identity provider, and a second one would take its refused token for no session at all.
        let maybe_restored = self
          .answer_in_time(async move { restoring.restore().await }, attempt, Wake::Ignored)
          .await;
        if self.core_round.try_get_untracked() != Some(round) {
          return;
        }
        let Some(restored) = maybe_restored else {
          attempt = attempt.saturating_add(1);
          continue;
        };
        match restore_verdict(restored) {
          // Nothing goes out yet: the session's user may not be the cached one.
          RestoreVerdict::SignedIn => needs_restore = false,
          RestoreVerdict::NoVerdict => {},
          RestoreVerdict::Over(maybe_problem) => return self.session_over(maybe_problem, cached.id).await,
        }
      }
      if !needs_restore {
        let asking = auth.clone();
        let maybe_answer = self
          .answer_in_time(async move { asking.api_get(ME_PATH).await }, attempt, Wake::Cuts)
          .await;
        if self.core_round.try_get_untracked() != Some(round) {
          return;
        }
        let Some(answer) = maybe_answer else {
          attempt = attempt.saturating_add(1);
          continue;
        };
        match account_verdict(answer, has_restored_for_refusal) {
          AccountVerdict::Confirmed(me) => return self.account_confirmed(me, &cached).await,
          AccountVerdict::NoVerdict => {},
          AccountVerdict::RestoreFirst => {
            has_restored_for_refusal = true;
            needs_restore = true;
            continue;
          },
          AccountVerdict::Refused(problem) => return self.account_refused(problem, cached.id).await,
          AccountVerdict::SessionOver => {
            return self
              .session_over(Some(LoginProblem::SessionExpired), cached.id)
              .await;
          },
        }
      }
      self.wait_for_next_account_check(attempt).await;
      attempt = attempt.saturating_add(1);
      if self.core_round.try_get_untracked() != Some(round) {
        return;
      }
    }
  }

  /// The network is back or the page returned: the check of a cached account asks again now.
  pub fn check_account_now(&self) {
    if let Some(wake) = self
      .maybe_check_wake
      .try_get_value()
      .and_then(|slot| slot.borrow_mut().take())
    {
      // The wait may have ended already.
      let _ = wake.send(());
    }
  }

  async fn wait_for_next_account_check(&self, attempt: u32) {
    let delay = account_check_delay(attempt, clock::now_ms());
    select(self.check_woken(), Box::pin(platform::sleep(delay))).await;
  }

  /// `work`'s answer, or `None` when it took too long or (with [`Wake::Cuts`]) the network came back.
  /// The work runs on in any case.
  async fn answer_in_time<T: 'static>(
    &self,
    work: impl Future<Output = T> + 'static,
    attempt: u32,
    wake: Wake,
  ) -> Option<T> {
    let (sender, answer) = oneshot::channel();
    spawn_local(async move {
      // Given up on: nobody listens any more.
      let _ = sender.send(work.await);
    });
    let patience = Box::pin(platform::sleep(account_answer_patience(attempt)));
    let maybe_answer = match wake {
      Wake::Cuts => match select(answer, select(self.check_woken(), patience)).await {
        Either::Left((answer, _)) => Some(answer),
        Either::Right(_) => None,
      },
      Wake::Ignored => match select(answer, patience).await {
        Either::Left((answer, _)) => Some(answer),
        Either::Right(_) => None,
      },
    };
    maybe_answer.and_then(Result::ok)
  }

  /// Resolves on the next [`Self::check_account_now`].
  fn check_woken(&self) -> oneshot::Receiver<()> {
    let (wake, woken) = oneshot::channel();
    if let Some(slot) = self.maybe_check_wake.try_get_value() {
      *slot.borrow_mut() = Some(wake);
    }
    woken
  }

  /// The session is over before the server said anything about the account. The shell may not be
  /// up yet, so the pictures go by the cached account.
  async fn session_over(&self, maybe_problem: Option<LoginProblem>, cached_user: UserId) {
    crate::persisted_media::clear(Some(cached_user)).await;
    self.sign_out_with(maybe_problem);
  }

  /// The server answered for the cached account's session. The same person: their data stays on
  /// screen, a changed name or role replaces the old one. Another account: nothing of the cached
  /// account stays (the server was reset, the identity provider maps people differently), and the
  /// start goes on as a sign-in of who the server says this is.
  async fn account_confirmed(&self, me: GetMeResponse, cached: &GetMeResponse) {
    self.is_account_unconfirmed.set_if_alive(false);
    if me.id != cached.id {
      tracing::warn!("the server names another account than the cached one");
      self.next_core_round();
      self.state.set_if_alive(SessionState::Checking);
      self.close_account_data(cached.id).await;
      self.start_core(me).await;
      return;
    }
    self.authenticator().remember_account(&me).await;
    self.maybe_confirmed.set_if_alive(Some(me.clone()));
    // Confirmed: the core gets its token and connects, and what waited goes out.
    if let Err(error) = remote_backend::network_changed().await {
      tracing::debug!(%error, "could not tell the core about the confirmed account");
    }
    // Rebuilds the shell: only when something changed since the last start, and usually
    // before anybody did anything in it. Name and role are read when the shell is built.
    if self
      .state
      .try_get_untracked()
      .is_some_and(|state| matches!(&state, SessionState::Ready(shown) if *shown != me))
    {
      self.state.set_if_alive(SessionState::Ready(me));
    }
  }

  /// The server refuses the cached account (deleted, deactivated, no role, the session ended): the
  /// data goes off the screen and the sign-in screen says why, as without a cached account. The
  /// tokens stay as they would have then; the screen offers to sign out or in again.
  async fn account_refused(&self, problem: LoginProblem, cached_user: UserId) {
    tracing::warn!(?problem, "the server refused the cached account");
    self.next_core_round();
    self.is_account_unconfirmed.set_if_alive(false);
    self.close_account_data(cached_user).await;
    self.state.set_if_alive(SessionState::SignedOut(Some(problem)));
  }

  /// Stops the core and removes what shows the account's data outside of the encrypted store.
  async fn close_account_data(&self, user: UserId) {
    crate::persisted_media::clear(Some(user)).await;
    if let Err(error) = remote_backend::stop_core().await {
      tracing::warn!(%error, "could not stop the client core");
    }
    self.authenticator().forget_account().await;
  }

  /// The server confirmed `me` and the core is ready: into the app, and the next start can
  /// begin here.
  fn enter_app(&self, me: GetMeResponse) {
    let auth = self.authenticator();
    let remembered = me.clone();
    spawn_local(async move { auth.remember_account(&remembered).await });
    self.state.set_if_alive(SessionState::Ready(me));
  }

  fn next_core_round(&self) -> u64 {
    let round = self.core_round.try_get_untracked().unwrap_or(0).wrapping_add(1);
    self.core_round.set_if_alive(round);
    round
  }

  /// Checks the server the user typed (desktop); its answer is shown for confirmation.
  pub fn check_server(&self, address: String) {
    self.is_server_from_link.set(false);
    self.run_server_check(address);
  }

  /// "Sign in there" on a link: the same check, and the confirmation warns that the user didn't
  /// enter this server.
  pub fn check_server_from_link(&self, address: String) {
    self.is_server_from_link.set(true);
    self.run_server_check(address);
  }

  fn run_server_check(&self, address: String) {
    let control = *self;
    control.is_checking_server.set(true);
    spawn_local(async move {
      let outcome = tauri::server_check(&address).await;
      control.is_checking_server.set_if_alive(false);
      control.state.set_if_alive(match outcome {
        Ok(check) => SessionState::ConfirmServer(Box::new(check)),
        Err(problem) => SessionState::NeedsServer {
          maybe_address: Some(address),
          maybe_problem: Some(problem),
        },
      });
    });
  }

  /// The user confirmed the checked server: it becomes the account's server and sign-in starts.
  /// `look` styles the browser's page after the sign-in.
  pub fn confirm_server(&self, check: ServerCheck, look: SignInLook) {
    let control = *self;
    control.is_checking_server.set(true);
    spawn_local(async move {
      let outcome = tauri::server_confirm(&check.server.origin).await;
      control.is_checking_server.set_if_alive(false);
      match outcome {
        Ok(server) => {
          crate::brand::show_server_brand(check.brand, &check.server.origin);
          control.server.set_if_alive(Some(server));
          control.sign_in(look);
        },
        Err(problem) => control.state.set_if_alive(SessionState::NeedsServer {
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
        control.state.set_if_alive(SessionState::NeedsServer {
          maybe_address: None,
          maybe_problem: Some(problem),
        });
        return;
      }
      control.server.set_if_alive(None);
      control.state.set_if_alive(SessionState::NeedsServer {
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
        self.state.set_if_alive(SessionState::NeedsServer {
          maybe_address: None,
          maybe_problem: Some(problem),
        });
        return false;
      },
    };
    self.is_server_locked.set_if_alive(server_state.is_locked);
    if let Some(server) = server_state.maybe_active {
      self.server.set_if_alive(Some(server));
      return true;
    }
    match server_state.maybe_managed_address {
      Some(address) => self.check_server(address),
      None => self.state.set_if_alive(SessionState::NeedsServer {
        maybe_address: None,
        maybe_problem: None,
      }),
    }
    false
  }

  /// Signs in with the identity provider (desktop: in the system browser, whose page after the
  /// sign-in gets `look`).
  pub fn sign_in(&self, look: SignInLook) {
    let control = *self;
    control.state.set(SessionState::SigningIn);
    spawn_local(async move {
      match control.authenticator().login(look).await {
        Ok(()) => control.load_account().await,
        Err(error) => {
          if error != AuthError::Cancelled {
            tracing::warn!(%error, "sign-in failed");
          }
          control
            .state
            .set_if_alive(SessionState::SignedOut(Some(problem_from_auth(&error))));
        },
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
      control.is_local_signing_in.set_if_alive(false);
      match outcome {
        Ok(()) => control.load_account().await,
        Err(error) => control.local_error.set_if_alive(Some(local_problem(&error))),
      }
    });
  }

  /// Stops waiting for the system browser (desktop). The pending sign-in then ends as cancelled.
  pub fn cancel_sign_in(&self) {
    self.authenticator().cancel_login();
  }

  pub fn sign_out(&self) {
    self.sign_out_with(None);
  }

  /// The identity provider refused the sign-in while the app ran: like [`Self::sign_out`], and the
  /// sign-in screen says the session expired.
  pub fn end_expired_session(&self) {
    self.sign_out_with(Some(LoginProblem::SessionExpired));
  }

  fn sign_out_with(&self, maybe_problem: Option<LoginProblem>) {
    let control = *self;
    let maybe_user = control.state.with_untracked(SessionState::maybe_user);
    control.next_core_round();
    control.is_account_unconfirmed.set(false);
    spawn_local(async move {
      // Pictures of this account don't outlive the session on this device (#749).
      crate::persisted_media::clear(maybe_user).await;
      if tauri::has_core()
        && let Err(error) = remote_backend::stop_core().await
      {
        tracing::warn!(%error, "could not stop the client core");
      }
      if let Err(error) = control.authenticator().logout().await {
        tracing::warn!(%error, "sign-out did not finish cleanly");
      }
      control.state.set_if_alive(SessionState::SignedOut(maybe_problem));
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
    let round = self.next_core_round();
    self.check_core(me, clock::now_ms(), round).await;
  }

  async fn check_core(&self, me: GetMeResponse, started_ms: i64, round: u64) {
    if self.core_round.get_untracked() != round {
      return;
    }
    let outcome = remote_backend::core_status(me.id).await;
    // Signed out or started again meanwhile: this answer is stale.
    if self.core_round.try_get_untracked() != Some(round) {
      return;
    }
    match outcome {
      Ok(CoreStatus::Ready { .. }) => self.enter_app(me),
      Ok(CoreStatus::NeedsEnrollment {
        has_existing_identity,
        was_data_lost,
        can_restore_on_device,
      }) => {
        self.state.set_if_alive(SessionState::NeedsEnrollment {
          me,
          has_existing_identity,
          was_data_lost,
          can_restore_on_device,
        });
      },
      Ok(CoreStatus::Failed { message }) => self
        .state
        .set_if_alive(SessionState::SignedOut(Some(LoginProblem::CoreFailed(message)))),
      Ok(CoreStatus::StoreUnreadable { problem }) => {
        self
          .state
          .set_if_alive(SessionState::SignedOut(Some(LoginProblem::StoreUnreadable(
            problem,
          ))))
      },
      Ok(CoreStatus::Stopped) => self
        .state
        .set_if_alive(SessionState::SignedOut(Some(LoginProblem::CoreStopped))),
      // The core is still waiting for an access token and retries on its own.
      Ok(CoreStatus::NotStarted) => self.wait_for_core(me, started_ms, round),
      Err(error) => self
        .state
        .set_if_alive(SessionState::SignedOut(Some(LoginProblem::Failed(error)))),
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
    spawn_local(async move {
      if tauri::is_available()
        && let Err(error) = remote_backend::retry_keychain().await
      {
        tracing::debug!(%error, "could not allow asking the keychain again");
      }
      control.load_account().await;
    });
  }

  /// The user confirmed setting this device up again after its local data turned out unreadable:
  /// the core sets the data aside (desktop) or deletes it (browser) and asks for setup (issue
  /// #160). A failure (another tab holds the database) shows as the core failure card with the
  /// reason, never as nothing.
  pub async fn reset_store(&self) {
    match remote_backend::reset_store().await {
      Ok(()) => {
        self.state.set_if_alive(SessionState::SigningIn);
        self.load_account().await;
      },
      Err(error) => self
        .state
        .set_if_alive(SessionState::SignedOut(Some(LoginProblem::CoreFailed(error)))),
    }
  }

  /// Creates this device's keys (or recovers them) with `secret`, then enters the app.
  pub fn enroll(&self, secret: EnrollSecret, device_name: String) {
    let control = *self;
    let SessionState::NeedsEnrollment { me, .. } = control.state.get_untracked() else {
      return;
    };
    control.is_enrolling.set(true);
    control.enroll_error.set(None);
    spawn_local(async move {
      let request = EnrollRequest { secret, device_name };
      match remote_backend::enroll(&request).await {
        Ok(()) => control.enter_app(me),
        Err(error) => control.enroll_error.set_if_alive(Some(error)),
      }
      control.is_enrolling.set_if_alive(false);
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
      self.enter_app(me);
    }
  }

  async fn load_account(&self) {
    let response = self.authenticator().api_get(ME_PATH).await;
    let outcome = response
      .map_err(|error| problem_from_auth(&error))
      .and_then(interpret_me_response);
    match outcome {
      // The desktop shell hosts the client core, whose enrollment replaces the demo welcome step.
      Ok(me) if tauri::has_core() => self.start_core(me).await,
      Ok(me) => {
        let is_ready = load_string(&device_ready_key(&me)).is_some();
        self.state.set_if_alive(if is_ready {
          SessionState::Ready(me)
        } else {
          SessionState::NeedsDeviceSetup(me)
        });
      },
      Err(problem) => {
        // A role-less account stays signed in at the identity provider; the login screen offers to sign out.
        self.state.set_if_alive(SessionState::SignedOut(Some(problem)));
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
      "account_deactivated" => LocalSignInProblem::Deactivated,
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
    AuthError::AuthorizationRefused(code) => LoginProblem::ProviderRefused {
      kind: AuthorizationErrorKind::from_code(code),
      code: code.clone(),
    },
    other => LoginProblem::Failed(other.to_string()),
  }
}

fn restore_verdict(outcome: Result<bool, AuthError>) -> RestoreVerdict {
  match outcome {
    Ok(true) => RestoreVerdict::SignedIn,
    Ok(false) => RestoreVerdict::Over(None),
    // The provider refused the stored sign-in, or nothing is stored any more.
    Err(AuthError::Denied(_) | AuthError::NotSignedIn) => {
      RestoreVerdict::Over(Some(LoginProblem::SessionExpired))
    },
    Err(error @ (AuthError::StateMismatch | AuthError::AuthorizationRefused(_))) => {
      RestoreVerdict::Over(Some(problem_from_auth(&error)))
    },
    Err(
      AuthError::Network(_)
      | AuthError::InvalidResponse(_)
      | AuthError::Unsupported(_)
      | AuthError::Cancelled,
    ) => RestoreVerdict::NoVerdict,
  }
}

/// Only an answer of the server itself refuses anybody: 401 or 403 with the server's error body. A
/// proxy's error page, a 5xx, a captive portal's page with status 200 and no answer at all are no
/// verdict.
fn account_verdict(outcome: Result<ApiResponse, AuthError>, has_restored: bool) -> AccountVerdict {
  let response = match outcome {
    Ok(response) => response,
    Err(AuthError::NotSignedIn) => return AccountVerdict::RestoreFirst,
    // The token ran out meanwhile, and its refresh was refused: the stored one is gone.
    Err(AuthError::Denied(_)) => return AccountVerdict::SessionOver,
    Err(_) => return AccountVerdict::NoVerdict,
  };
  if response.status == 200 {
    return match serde_json::from_str(&response.body) {
      Ok(me) => AccountVerdict::Confirmed(me),
      Err(_) => AccountVerdict::NoVerdict,
    };
  }
  if !matches!(response.status, 401 | 403) {
    return AccountVerdict::NoVerdict;
  }
  let Ok(body) = serde_json::from_str::<ErrorBody>(&response.body) else {
    return AccountVerdict::NoVerdict;
  };
  let is_final = matches!(
    body.error_code,
    ErrorCode::SessionEnded
      | ErrorCode::SessionTimedOut
      | ErrorCode::AccountDeleted
      | ErrorCode::AccountDeactivated
  );
  if response.status == 401 && !is_final && !has_restored {
    return AccountVerdict::RestoreFirst;
  }
  match interpret_me_response(response) {
    Ok(me) => AccountVerdict::Confirmed(me),
    Err(problem) => AccountVerdict::Refused(problem),
  }
}

/// The wait after `attempt` checks without a verdict. `jitter_seed` is any number that differs
/// between tabs and devices (the clock will do).
fn account_check_delay(attempt: u32, jitter_seed: i64) -> Duration {
  let backoff_ms = ACCOUNT_CHECK_RETRY_MS
    .saturating_mul(1_u64 << attempt.min(16))
    .min(ACCOUNT_CHECK_RETRY_MAX_MS);
  Duration::from_millis(backoff_ms.saturating_add(jitter_seed.rem_euclid(1_000).unsigned_abs()))
}

/// How long the check waits for an answer at this attempt.
fn account_answer_patience(attempt: u32) -> Duration {
  Duration::from_millis(
    ACCOUNT_ANSWER_PATIENCE_MS
      .saturating_mul(1_u64 << attempt.min(16))
      .min(ACCOUNT_ANSWER_PATIENCE_MAX_MS),
  )
}

/// Turns the answer of `GET /api/v1/me` into the account or the reason it cannot be used.
pub fn interpret_me_response(response: ApiResponse) -> Result<GetMeResponse, LoginProblem> {
  match response.status {
    200 => serde_json::from_str(&response.body)
      .map_err(|error| LoginProblem::Failed(format!("unreadable account response: {error}"))),
    status => match serde_json::from_str::<ErrorBody>(&response.body) {
      Ok(body) if body.error_code == ErrorCode::NoTeamcraftRole => Err(LoginProblem::NoTeamcraftRole),
      Ok(body) if body.error_code == ErrorCode::AccountDeactivated => Err(LoginProblem::AccountDeactivated),
      Ok(body) if body.error_code == ErrorCode::AccountDeleted => Err(LoginProblem::AccountDeleted),
      Ok(body) if body.error_code == ErrorCode::SessionEnded => Err(LoginProblem::SessionEnded),
      Ok(body) if body.error_code == ErrorCode::SessionTimedOut => Err(LoginProblem::SessionTimedOut),
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
  fn a_deactivated_account_gets_its_own_problem() {
    let body = r#"{"error_code":"account_deactivated","message":"the account is deactivated"}"#;
    let outcome = interpret_me_response(ApiResponse {
      status: 403,
      body: body.into(),
    });
    assert_eq!(outcome, Err(LoginProblem::AccountDeactivated));
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
      ("account_deactivated", LocalSignInProblem::Deactivated),
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
    assert_eq!(
      problem_from_auth(&AuthError::AuthorizationRefused("access_denied".into())),
      LoginProblem::ProviderRefused {
        kind: AuthorizationErrorKind::AccessDenied,
        code: "access_denied".into(),
      }
    );
  }

  mod cached_start {
    use super::*;

    const ALICE: &str = r#"{"id":"42","username":"alice","display_name":"Alice","role":"owner"}"#;

    fn answer(status: u16, body: &str) -> Result<ApiResponse, AuthError> {
      Ok(ApiResponse {
        status,
        body: body.into(),
      })
    }

    fn error_body(code: &str) -> String {
      format!(r#"{{"error_code":"{code}","message":"no"}}"#)
    }

    #[test]
    fn no_network_failure_ends_a_session() {
      for error in [
        AuthError::Network("down".into()),
        AuthError::InvalidResponse("<html>".into()),
        AuthError::Unsupported("keyring: locked".into()),
        AuthError::Cancelled,
      ] {
        assert_eq!(
          restore_verdict(Err(error.clone())),
          RestoreVerdict::NoVerdict,
          "{error}"
        );
        assert_eq!(
          account_verdict(Err(error.clone()), false),
          AccountVerdict::NoVerdict,
          "{error}"
        );
      }
    }

    #[test]
    fn a_refused_refresh_token_ends_the_session() {
      assert_eq!(
        restore_verdict(Err(AuthError::Denied("invalid_grant".into()))),
        RestoreVerdict::Over(Some(LoginProblem::SessionExpired))
      );
      assert_eq!(
        restore_verdict(Err(AuthError::NotSignedIn)),
        RestoreVerdict::Over(Some(LoginProblem::SessionExpired))
      );
      assert_eq!(restore_verdict(Ok(false)), RestoreVerdict::Over(None));
      assert_eq!(restore_verdict(Ok(true)), RestoreVerdict::SignedIn);
    }

    #[test]
    fn what_is_not_the_server_speaking_decides_nothing() {
      for (status, body) in [
        (200, "<html>Sign in to the hotel Wi-Fi</html>"),
        (502, "<html>Bad gateway</html>"),
        (503, r#"{"error_code":"unavailable","message":"later"}"#),
        (429, r#"{"error_code":"rate_limited","message":"later"}"#),
        (401, "<html>Proxy authentication required</html>"),
        (403, "<html>Blocked by policy</html>"),
        (404, "<html>Not found</html>"),
      ] {
        assert_eq!(
          account_verdict(answer(status, body), false),
          AccountVerdict::NoVerdict,
          "{status} {body}"
        );
      }
    }

    #[test]
    fn the_server_refuses_what_it_says_it_refuses() {
      let deleted = account_verdict(answer(401, &error_body("account_deleted")), false);
      assert_eq!(deleted, AccountVerdict::Refused(LoginProblem::AccountDeleted));
      assert_eq!(
        account_verdict(answer(403, &error_body("account_deactivated")), false),
        AccountVerdict::Refused(LoginProblem::AccountDeactivated)
      );
      assert_eq!(
        account_verdict(answer(403, &error_body("no_teamcraft_role")), false),
        AccountVerdict::Refused(LoginProblem::NoTeamcraftRole)
      );
      assert_eq!(
        account_verdict(answer(401, &error_body("session_ended")), false),
        AccountVerdict::Refused(LoginProblem::SessionEnded)
      );
      assert_eq!(
        account_verdict(answer(401, &error_body("session_timed_out")), false),
        AccountVerdict::Refused(LoginProblem::SessionTimedOut)
      );
    }

    /// A token that ran out during a suspend or by a wrong clock gets one new token before
    /// anybody is told anything; refused again, the server's answer stands.
    #[test]
    fn a_refused_token_is_renewed_once() {
      let refused = error_body("invalid_token");
      assert_eq!(
        account_verdict(answer(401, &refused), false),
        AccountVerdict::RestoreFirst
      );
      assert_eq!(
        account_verdict(answer(401, &refused), true),
        AccountVerdict::Refused(LoginProblem::Failed("invalid_token (401)".into()))
      );
    }

    #[test]
    fn the_account_answer_is_read() {
      let AccountVerdict::Confirmed(me) = account_verdict(answer(200, ALICE), false) else {
        panic!("expected the account");
      };
      assert_eq!(me.display_name, "Alice");
    }

    /// Restored, then no network, then a refresh the provider refuses: the session is over, also
    /// on a retry that only hears the network error of a session with nothing left in it.
    #[test]
    fn a_refresh_refused_while_asking_ends_the_session() {
      assert_eq!(
        account_verdict(Err(AuthError::Denied("invalid_grant".into())), false),
        AccountVerdict::SessionOver
      );
      assert_eq!(
        account_verdict(Err(AuthError::NotSignedIn), false),
        AccountVerdict::RestoreFirst
      );
    }

    #[test]
    fn the_patience_for_an_answer_grows_up_to_its_maximum() {
      assert_eq!(account_answer_patience(0).as_millis(), 10_000);
      assert_eq!(account_answer_patience(1).as_millis(), 20_000);
      assert_eq!(account_answer_patience(u32::MAX).as_millis(), 60_000);
    }

    #[test]
    fn the_wait_doubles_up_to_its_maximum_with_a_part_of_a_second_on_top() {
      let wait = |attempt, seed| account_check_delay(attempt, seed).as_millis();
      assert_eq!(wait(0, 0), 2_000);
      assert_eq!(wait(1, 0), 4_000);
      assert_eq!(wait(5, 0), 30_000);
      assert_eq!(wait(u32::MAX, 0), 30_000);
      assert_eq!(wait(0, 1_999), 2_999);
      // A clock before 1970 is no reason to wait less.
      assert!(wait(0, -1) >= 2_000);
    }
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
