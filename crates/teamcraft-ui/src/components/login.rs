//! Screens around sign-in: checking the session, the login screen with its problems, the welcome
//! screen of a new device, and the gate that decides which one to show.

use std::rc::Rc;

use leptos::prelude::*;
use teamcraft_proto::GetMeResponse;

use crate::auth::tauri;
use crate::auth::{self, AppConfig};
use crate::backend::Backend;
use crate::clock::now_ms;
use crate::components::app::Shell;
use crate::components::icons::{Icon, IconName};
use crate::hybrid::Hybrid;
use crate::i18n::use_i18n;
use crate::meetings::{MeetingsClient, MeetingsHub};
use crate::notify_sync::NotificationSync;
use crate::remote_backend::RemoteChat;
use crate::server_backend::{LoadError, RestChat};
use crate::session::{LoginProblem, SessionControl, SessionState};
use crate::settings::{BackendMode, Settings};

/// Shows the right screen for the sign-in state and the chat shell once the user is in.
#[component]
pub fn SessionGate(settings: Settings) -> impl IntoView {
  let config = AppConfig::load();
  let control = SessionControl::new(auth::detect(config));
  provide_context(control);
  control.start();

  view! {
    {move || match control.state.get() {
      SessionState::Checking => view! { <CheckingScreen /> }.into_any(),
      SessionState::SignedOut(problem) => view! { <LoginScreen problem=problem control=control /> }.into_any(),
      SessionState::SigningIn => view! { <SigningInScreen control=control /> }.into_any(),
      SessionState::NeedsEnrollment { me, has_existing_identity } => {
        view! { <EnrollScreen name=me.display_name has_existing_identity=has_existing_identity control=control /> }.into_any()
      },
      SessionState::NeedsDeviceSetup(me) => view! { <WelcomeScreen name=me.display_name control=control /> }.into_any(),
      SessionState::Ready(me) => view! { <DataGate me=me settings=settings control=control /> }.into_any(),
    }}
  }
}

#[component]
pub(crate) fn AuthFrame(children: Children) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <main class="auth-page">
      <section class="auth-card">
        <div class="auth-brand" translate="no">
          <span class="auth-logo" aria-hidden="true">"T"</span>
          <span>"TeamCraft"</span>
        </div>
        {children()}
        <p class="auth-foot muted-text small">{move || i18n.t("login-footer")}</p>
      </section>
    </main>
  }
}

#[component]
fn CheckingScreen() -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <AuthFrame>
      <p class="auth-status" role="status">
        <span class="spinner" aria-hidden="true"></span>
        <span>{move || i18n.t("login-checking")}</span>
      </p>
    </AuthFrame>
  }
}

#[component]
fn SigningInScreen(control: SessionControl) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <AuthFrame>
      <h1>{move || i18n.t("login-signing-in-title")}</h1>
      <p class="auth-status" role="status">
        <span class="spinner" aria-hidden="true"></span>
        <span>{move || i18n.t("login-signing-in")}</span>
      </p>
      <button class="btn" on:click=move |_| control.cancel_sign_in()>
        {move || i18n.t("cancel")}
      </button>
    </AuthFrame>
  }
}

#[component]
fn LoginScreen(problem: Option<LoginProblem>, control: SessionControl) -> impl IntoView {
  let i18n = use_i18n();
  let is_role_problem = problem == Some(LoginProblem::NoTeamcraftRole);
  let detail = match &problem {
    Some(LoginProblem::Failed(text) | LoginProblem::CoreFailed(text)) => Some(text.clone()),
    _ => None,
  };
  let (title_key, body_key) = match &problem {
    Some(LoginProblem::NoTeamcraftRole) => (Some("login-no-role-title"), Some("login-no-role-body")),
    Some(LoginProblem::ServerUnreachable) => {
      (Some("login-unreachable-title"), Some("login-unreachable-body"))
    },
    Some(LoginProblem::Cancelled) => (Some("login-cancelled-title"), Some("login-cancelled-body")),
    Some(LoginProblem::Failed(_)) => (Some("login-failed-title"), Some("login-failed-body")),
    Some(LoginProblem::CoreFailed(_)) => (Some("login-core-failed-title"), Some("login-core-failed-body")),
    None => (None, None),
  };

  view! {
    <AuthFrame>
      <h1>{move || i18n.t("login-title")}</h1>
      <p class="muted-text">{move || i18n.t("login-subtitle")}</p>
      {title_key
        .zip(body_key)
        .map(|(title, body)| {
          view! {
            <div class="auth-alert" role="alert">
              <Icon name=IconName::Alert />
              <div>
                <strong>{move || i18n.t(title)}</strong>
                <p>{move || i18n.t(body)}</p>
                {detail.map(|text| view! { <p class="small mono" translate="no">{text}</p> })}
              </div>
            </div>
          }
        })}
      <div class="auth-actions">
        {if is_role_problem {
          view! {
            <button class="btn btn-primary" data-autofocus on:click=move |_| control.sign_out()>
              {move || i18n.t("login-use-other-account")}
            </button>
          }
          .into_any()
        } else {
          view! {
            <button class="btn btn-primary" data-autofocus on:click=move |_| control.sign_in()>
              {move || i18n.t("login-sign-in")}
            </button>
          }
          .into_any()
        }}
      </div>
      <button
        class="link-btn"
        on:click=move |_| {
          BackendMode::Mock.save();
          if let Some(window) = web_sys::window() {
            drop(window.location().reload());
          }
        }
      >
        {move || i18n.t("login-use-demo")}
      </button>
    </AuthFrame>
  }
}

/// First device of an account: explains what happens next. Creating the passkey is a placeholder
/// until `teamcraft-core` provides enrollment (e2ee.md §3).
#[component]
fn WelcomeScreen(name: String, control: SessionControl) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <AuthFrame>
      <h1 translate="no">{move || i18n.t_with("welcome-title", &[("name", &name)])}</h1>
      <p class="muted-text">{move || i18n.t("welcome-subtitle")}</p>
      <ol class="welcome-steps">
        <li>
          <strong>{move || i18n.t("welcome-step-keys")}</strong>
          <span class="muted-text">{move || i18n.t("welcome-step-keys-hint")}</span>
        </li>
        <li>
          <strong>{move || i18n.t("welcome-step-passkey")}</strong>
          <span class="muted-text">{move || i18n.t("welcome-step-passkey-hint")}</span>
        </li>
      </ol>
      <div class="auth-actions">
        <button class="btn" disabled title=move || i18n.t("admin-soon")>
          <Icon name=IconName::Key />
          <span>{move || i18n.t("welcome-create-passkey")}</span>
        </button>
        <button class="btn btn-primary" data-autofocus on:click=move |_| control.finish_device_setup()>
          {move || i18n.t("welcome-continue")}
        </button>
      </div>
      <p class="muted-text small">{move || i18n.t("welcome-demo-note")}</p>
      <button class="link-btn" on:click=move |_| control.sign_out()>
        {move || i18n.t("menu-log-out")}
      </button>
    </AuthFrame>
  }
}

/// The desktop app reads chat data from the client core in the shell (encrypted store, sync); the
/// browser build reads spaces and conversations over REST. Everything else is demo data for now.
async fn load_backend(
  auth: Rc<dyn crate::auth::Authenticator>,
  me: &GetMeResponse,
) -> Result<Rc<dyn Backend>, LoadError> {
  if tauri::is_available() {
    let chat = RemoteChat::connect().await.map_err(|reason| LoadError::Parse {
      path: "core_snapshot".to_owned(),
      reason,
    })?;
    return Ok(Rc::new(Hybrid::with_live(chat, me.role.clone(), now_ms(), auth)));
  }
  let chat = RestChat::load(auth.as_ref(), me, now_ms()).await?;
  Ok(Rc::new(Hybrid::with_live(chat, me.role.clone(), now_ms(), auth)))
}

/// Loads spaces, conversations and members from the server, then shows the shell. A failed load
/// offers a retry instead of an empty app.
#[component]
fn DataGate(me: GetMeResponse, settings: Settings, control: SessionControl) -> impl IntoView {
  let i18n = use_i18n();
  let backend = RwSignal::new_local(None::<Result<Rc<dyn Backend>, LoadError>>);
  let attempt = RwSignal::new(0_u32);
  let me = StoredValue::new(me);

  Effect::new(move |_| {
    attempt.track();
    backend.set(None);
    let auth = control.authenticator();
    let me = me.get_value();
    leptos::task::spawn_local(async move {
      backend.set(Some(load_backend(auth, &me).await));
    });
  });

  view! {
    {move || match backend.get() {
      None => view! { <CheckingScreen /> }.into_any(),
      Some(Ok(loaded)) => {
        provide_context(StoredValue::new_local(NotificationSync::new(control.authenticator())));
        provide_context(MeetingsHub::new(MeetingsClient::new(control.authenticator())));
        view! { <Shell backend=loaded settings=settings /> }.into_any()
      },
      Some(Err(error)) => {
        tracing::warn!(%error, "could not load the account data");
        let detail = error.to_string();
        view! {
          <AuthFrame>
            <div class="auth-alert" role="alert">
              <Icon name=IconName::Alert />
              <div>
                <strong>{move || i18n.t("data-failed-title")}</strong>
                <p>{move || i18n.t("data-failed-body")}</p>
                <p class="small mono" translate="no">{detail}</p>
              </div>
            </div>
            <div class="auth-actions">
              <button class="btn btn-primary" data-autofocus on:click=move |_| attempt.update(|n| *n += 1)>
                {move || i18n.t("data-retry")}
              </button>
              <button class="btn" on:click=move |_| control.sign_out()>{move || i18n.t("menu-log-out")}</button>
            </div>
          </AuthFrame>
        }
        .into_any()
      },
    }}
  }
}

/// First run on this device (desktop): create the encryption keys, or recover them when the account
/// already has some. The passphrase wraps the key backup (e2ee.md §6.2).
#[component]
fn EnrollScreen(name: String, has_existing_identity: bool, control: SessionControl) -> impl IntoView {
  let i18n = use_i18n();
  let passphrase = RwSignal::new(String::new());
  let confirm = RwSignal::new(String::new());
  let device_name = RwSignal::new(default_device_name());
  let mismatch =
    move || !has_existing_identity && !confirm.get().is_empty() && confirm.get() != passphrase.get();
  let too_short = move || {
    !has_existing_identity
      && !passphrase.get().is_empty()
      && passphrase.get().chars().count() < MIN_PASSPHRASE_CHARS
  };
  let can_submit = move || {
    !control.is_enrolling.get()
      && !passphrase.get().is_empty()
      && (has_existing_identity
        || (passphrase.get().chars().count() >= MIN_PASSPHRASE_CHARS && confirm.get() == passphrase.get()))
  };
  let submit = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    if can_submit() {
      control.enroll(
        passphrase.get_untracked(),
        device_name.get_untracked().trim().to_owned(),
      );
    }
  };

  view! {
    <AuthFrame>
      <h1 translate="no">{move || i18n.t_with("enroll-title", &[("name", &name)])}</h1>
      <p class="muted-text">
        {move || if has_existing_identity { i18n.t("enroll-recover-subtitle") } else { i18n.t("enroll-new-subtitle") }}
      </p>
      <form class="auth-form" on:submit=submit>
        <div class="field">
          <label for="enroll-passphrase">
            {move || if has_existing_identity { i18n.t("enroll-passphrase-recover") } else { i18n.t("enroll-passphrase-new") }}
          </label>
          <input
            id="enroll-passphrase"
            class="input"
            type="password"
            name="passphrase"
            autocomplete=if has_existing_identity { "current-password" } else { "new-password" }
            data-autofocus
            aria-describedby="enroll-passphrase-hint"
            prop:value=move || passphrase.get()
            on:input=move |ev| passphrase.set(event_target_value(&ev))
          />
          <p id="enroll-passphrase-hint" class="muted-text small" class:warn-text=too_short>
            {move || {
              if has_existing_identity {
                i18n.t("enroll-recover-hint")
              } else {
                i18n.t_with("enroll-new-hint", &[("count", &MIN_PASSPHRASE_CHARS.to_string())])
              }
            }}
          </p>
        </div>
        <Show when=move || !has_existing_identity fallback=|| ()>
          <div class="field">
            <label for="enroll-confirm">{move || i18n.t("enroll-confirm")}</label>
            <input
              id="enroll-confirm"
              class="input"
              type="password"
              name="passphrase-confirm"
              autocomplete="new-password"
              aria-invalid=move || mismatch().to_string()
              prop:value=move || confirm.get()
              on:input=move |ev| confirm.set(event_target_value(&ev))
            />
            <Show when=mismatch fallback=|| ()>
              <p class="field-error" role="alert">{move || i18n.t("enroll-mismatch")}</p>
            </Show>
          </div>
        </Show>
        <div class="field">
          <label for="enroll-device">{move || i18n.t("enroll-device-name")}</label>
          <input
            id="enroll-device"
            class="input"
            name="device"
            autocomplete="off"
            prop:value=move || device_name.get()
            on:input=move |ev| device_name.set(event_target_value(&ev))
          />
        </div>
        <Show when=move || !has_existing_identity fallback=|| ()>
          <p class="auth-alert" role="note">
            <Icon name=IconName::Alert />
            <span>{move || i18n.t("enroll-warning")}</span>
          </p>
        </Show>
        {move || {
          control
            .enroll_error
            .get()
            .map(|message| {
              view! {
                <p class="field-error" role="alert">
                  {move || if has_existing_identity { i18n.t("enroll-recover-failed") } else { i18n.t("enroll-failed") }}
                  <span class="small mono" translate="no">{message}</span>
                </p>
              }
            })
        }}
        <div class="auth-actions">
          <button type="submit" class="btn btn-primary" disabled=move || !can_submit()>
            {move || {
              if control.is_enrolling.get() {
                i18n.t("enroll-working")
              } else if has_existing_identity {
                i18n.t("enroll-recover")
              } else {
                i18n.t("enroll-create")
              }
            }}
          </button>
          <button type="button" class="btn" on:click=move |_| control.sign_out()>{move || i18n.t("menu-log-out")}</button>
        </div>
      </form>
    </AuthFrame>
  }
}

/// Short passphrases are cheap to guess offline; the minimum is a UI rule, the cost of the key
/// derivation (Argon2id) does the rest.
const MIN_PASSPHRASE_CHARS: usize = 12;

fn default_device_name() -> String {
  let platform = web_sys::window()
    .and_then(|window| window.navigator().platform().ok())
    .unwrap_or_default();
  if platform.is_empty() {
    "Desktop".to_owned()
  } else {
    format!("Desktop ({platform})")
  }
}
