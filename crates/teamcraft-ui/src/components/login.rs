//! Screens around sign-in: checking the session, the login screen with its problems, the welcome
//! screen of a new device, and the gate that decides which one to show.

use std::rc::Rc;

use leptos::prelude::*;
use teamcraft_proto::GetMeResponse;

use crate::auth::{self, AppConfig};
use crate::backend::Backend;
use crate::clock::now_ms;
use crate::components::app::Shell;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::server_backend::{LoadError, ServerBackend};
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
      SessionState::NeedsDeviceSetup(me) => view! { <WelcomeScreen name=me.display_name control=control /> }.into_any(),
      SessionState::Ready(me) => view! { <DataGate me=me settings=settings control=control /> }.into_any(),
    }}
  }
}

#[component]
fn AuthFrame(children: Children) -> impl IntoView {
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
    Some(LoginProblem::Failed(text)) => Some(text.clone()),
    _ => None,
  };
  let (title_key, body_key) = match &problem {
    Some(LoginProblem::NoTeamcraftRole) => (Some("login-no-role-title"), Some("login-no-role-body")),
    Some(LoginProblem::ServerUnreachable) => {
      (Some("login-unreachable-title"), Some("login-unreachable-body"))
    },
    Some(LoginProblem::Cancelled) => (Some("login-cancelled-title"), Some("login-cancelled-body")),
    Some(LoginProblem::Failed(_)) => (Some("login-failed-title"), Some("login-failed-body")),
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
      let loaded = ServerBackend::load(auth.as_ref(), &me, now_ms()).await;
      backend.set(Some(loaded.map(|loaded| Rc::new(loaded) as Rc<dyn Backend>)));
    });
  });

  view! {
    {move || match backend.get() {
      None => view! { <CheckingScreen /> }.into_any(),
      Some(Ok(loaded)) => view! { <Shell backend=loaded settings=settings /> }.into_any(),
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
