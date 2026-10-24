//! Screens around sign-in: checking the session, the login screen with its problems, the welcome
//! screen of a new device, and the gate that decides which one to show.

use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::GetMeResponse;

use crate::auth::tauri::{self, ServerCheck, ServerProblem};
use crate::auth::{self, AppConfig};
use crate::backend::Backend;
use crate::brand::Brand;
use crate::clock::now_ms;
use crate::components::app::Shell;
use crate::components::bundle_banner::BundleBanner;
use crate::components::icons::{Icon, IconName};
use crate::components::security_ui::QrImage;
use crate::hybrid::Hybrid;
use crate::i18n::use_i18n;
use crate::meetings::{MeetingsClient, MeetingsHub};
use crate::notify_sync::NotificationSync;
use crate::reactive::use_interval;
use crate::remote_backend::{self, RemoteChat};
use crate::remote_calls::RemoteCalls;
use crate::server_backend::{LoadError, RestChat};
use crate::session::{LocalSignInProblem, LoginProblem, SessionControl, SessionState};
use crate::settings::Settings;
use teamcraft_client_api::remote::{CoreStatus, LinkStatus};

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
      SessionState::NeedsServer { maybe_address, maybe_problem } => {
        view! { <ServerEntryScreen maybe_address=maybe_address maybe_problem=maybe_problem control=control /> }.into_any()
      },
      SessionState::ConfirmServer(check) => view! { <ServerConfirmScreen check=*check control=control /> }.into_any(),
      SessionState::SignedOut(problem) => view! { <LoginScreen problem=problem control=control /> }.into_any(),
      SessionState::SigningIn => view! { <SigningInScreen control=control /> }.into_any(),
      SessionState::NeedsEnrollment { me, has_existing_identity, was_data_lost } => {
        view! { <EnrollScreen name=me.display_name has_existing_identity=has_existing_identity was_data_lost=was_data_lost control=control /> }.into_any()
      },
      SessionState::NeedsDeviceSetup(me) => view! { <WelcomeScreen name=me.display_name control=control /> }.into_any(),
      SessionState::Ready(me) => view! { <DataGate me=me settings=settings control=control /> }.into_any(),
    }}
  }
}

#[component]
pub(crate) fn AuthFrame(children: Children) -> impl IntoView {
  let i18n = use_i18n();
  let brand = Brand::use_current().0;
  let language = move || i18n.locale().tag();
  let name = move || brand.with(|view| view.display_name.clone());
  let links = move || {
    brand.with(|view| {
      [
        ("login-brand-support", view.links.maybe_support.clone()),
        ("login-brand-privacy", view.links.maybe_privacy.clone()),
        ("login-brand-terms", view.links.maybe_terms.clone()),
        ("login-brand-imprint", view.links.maybe_imprint.clone()),
      ]
      .into_iter()
      .filter_map(|(key, link)| link.map(|link| (key, link)))
      .collect::<Vec<_>>()
    })
  };
  view! {
    <main class="auth-page">
      <BundleBanner />
      <section class="auth-card">
        <div class="auth-brand" translate="no">
          {move || match brand.with(|view| view.asset("logo", false).map(str::to_owned)) {
            Some(light) => {
              let dark = brand.with(|view| view.asset("logo", true).map(str::to_owned)).filter(|dark| *dark != light);
              view! {
                <img class="auth-logo-img brand-light" src=light alt=name() />
                {dark.map(|dark| view! { <img class="auth-logo-img brand-dark" src=dark alt=name() /> })}
              }
                .into_any()
            },
            None => view! { <span class="auth-logo" aria-hidden="true">{move || name().chars().next().map(String::from).unwrap_or_default()}</span> }.into_any(),
          }}
          <span>{name}</span>
        </div>
        {move || {
          brand
            .with(|view| view.login_title(language()).map(str::to_owned))
            .map(|title| view! { <h2 class="auth-brand-title">{title}</h2> })
        }}
        {move || {
          brand
            .with(|view| view.login_text(language()).map(str::to_owned))
            .map(|text| view! { <p class="auth-brand-text muted-text">{text}</p> })
        }}
        {children()}
        <p class="auth-foot muted-text small">{move || i18n.t("login-footer")}</p>
        {move || {
          brand
            .with(|view| view.maybe_organization_name.clone())
            .map(|name| {
              view! {
                <p class="auth-organization muted-text small" translate="no">
                  {i18n.t_with("login-organization", &[("name", &name)])}
                </p>
              }
            })
        }}
        <nav class="auth-links small" aria-label=move || i18n.t("login-brand-links")>
          <For each=links key=|(key, link)| (*key, link.clone()) let:item>
            <a href=item.1.clone() target="_blank" rel="noopener noreferrer">{move || i18n.t(item.0)}</a>
          </For>
        </nav>
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
      <Show when=|| !tauri::is_available() fallback=|| ()>
        <LocalSignIn control=control />
      </Show>
      <ServerLine control=control />
      {demo_switch()}
    </AuthFrame>
  }
}

/// Desktop: which server the sign-in is for, and the way to another one.
#[component]
fn ServerLine(control: SessionControl) -> impl IntoView {
  let i18n = use_i18n();
  move || {
    control.server.get().map(|server| {
      view! {
        <p class="auth-server muted-text small">
          <span translate="no">{i18n.t_with("server-entry-current", &[("host", &server.host)])}</span>
          <Show when=move || !control.is_server_locked.get() fallback=|| ()>
            " "
            <button class="link-btn" on:click=move |_| control.change_server()>
              {move || i18n.t("server-entry-change")}
            </button>
          </Show>
        </p>
      }
    })
  }
}

/// Desktop, first start: the address of the server to sign in to.
#[component]
fn ServerEntryScreen(
  maybe_address: Option<String>,
  maybe_problem: Option<ServerProblem>,
  control: SessionControl,
) -> impl IntoView {
  let i18n = use_i18n();
  let address = RwSignal::new(maybe_address.unwrap_or_default());
  let submit = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    control.check_server(address.get_untracked());
  };
  view! {
    <AuthFrame>
      <h1>{move || i18n.t("server-entry-title")}</h1>
      <p class="muted-text">{move || i18n.t("server-entry-subtitle")}</p>
      <form class="auth-server-entry" on:submit=submit>
        <label for="server-address">{move || i18n.t("server-entry-label")}</label>
        <input
          id="server-address"
          class="input"
          type="text"
          inputmode="url"
          autocomplete="url"
          autocapitalize="off"
          spellcheck="false"
          placeholder="team.example.com"
          required
          data-autofocus
          prop:readonly=move || control.is_server_locked.get()
          prop:value=move || address.get()
          on:input=move |ev| address.set(event_target_value(&ev))
        />
        <p class="muted-text small">{move || i18n.t("server-entry-hint")}</p>
        {maybe_problem.map(|problem| {
          view! {
            <div class="auth-alert" role="alert">
              <Icon name=IconName::Alert />
              <div>
                <p>{move || i18n.t(&problem.key)}</p>
                <p class="small mono" translate="no">{problem.detail.clone()}</p>
              </div>
            </div>
          }
        })}
        <div class="auth-actions">
          <button type="submit" class="btn btn-primary" prop:disabled=move || control.is_checking_server.get()>
            <Show when=move || control.is_checking_server.get() fallback=|| ()>
              <span class="spinner" aria-hidden="true"></span>
            </Show>
            <span>{move || i18n.t("server-entry-continue")}</span>
          </button>
        </div>
      </form>
    </AuthFrame>
  }
}

/// Desktop: the server answered; its name and host are confirmed before the browser opens.
#[component]
fn ServerConfirmScreen(check: ServerCheck, control: SessionControl) -> impl IntoView {
  let i18n = use_i18n();
  let name = check.server.display_name.clone();
  let host = check.server.host.clone();
  let initial = name.chars().next().map(String::from).unwrap_or_default();
  let provider = check.identity_provider_host.clone();
  let warning_host = provider.clone();
  let is_elsewhere = check.is_identity_provider_elsewhere;
  view! {
    <AuthFrame>
      <h1>{move || i18n.t("server-entry-confirm-title")}</h1>
      <div class="auth-server-card">
        <span class="auth-logo" aria-hidden="true">{initial}</span>
        <div>
          <strong translate="no">{name}</strong>
          <p class="muted-text small mono" translate="no">{host}</p>
        </div>
      </div>
      <p class="auth-provider small">
        <span translate="no">{i18n.t_with("server-entry-provider", &[("host", &provider)])}</span>
      </p>
      {is_elsewhere
        .then(|| {
          view! {
            <div class="auth-alert" role="alert">
              <Icon name=IconName::Alert />
              <p>{i18n.t_with("server-entry-provider-elsewhere", &[("host", &warning_host)])}</p>
            </div>
          }
        })}
      <p class="muted-text">{move || i18n.t("server-entry-confirm-body")}</p>
      <div class="auth-actions">
        <button
          class="btn btn-primary"
          data-autofocus
          prop:disabled=move || control.is_checking_server.get()
          on:click=move |_| control.confirm_server(check.clone())
        >
          {move || i18n.t("server-entry-confirm")}
        </button>
        <Show when=move || !control.is_server_locked.get() fallback=|| ()>
          <button class="btn" on:click=move |_| control.change_server()>
            {move || i18n.t("server-entry-other")}
          </button>
        </Show>
      </div>
    </AuthFrame>
  }
}

/// Development builds offer the demo data on the login screen.
#[cfg(feature = "demo")]
fn demo_switch() -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <button
      class="link-btn"
      on:click=move |_| {
        crate::settings::BackendMode::Mock.save();
        if let Some(window) = web_sys::window() {
          drop(window.location().reload());
        }
      }
    >
      {move || i18n.t("login-use-demo")}
    </button>
  }
}

#[cfg(not(feature = "demo"))]
fn demo_switch() -> impl IntoView {}

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

/// The desktop app reads chat data from the client core in the shell (encrypted store, sync), the
/// browser build from the core in the page; without a core, spaces and conversations come over REST.
async fn load_backend(
  auth: Rc<dyn crate::auth::Authenticator>,
  me: &GetMeResponse,
) -> Result<Rc<dyn Backend>, LoadError> {
  if tauri::has_core() {
    let chat = RemoteChat::connect().await.map_err(|reason| LoadError::Parse {
      path: "core_snapshot".to_owned(),
      reason,
    })?;
    let hybrid = Hybrid::with_live(chat, me.role.clone(), auth);
    // Calls are optional: without them the app offers no calls.
    let hybrid = if tauri::is_available() {
      match RemoteCalls::connect().await {
        Ok(calls) => hybrid.with_calls(calls),
        Err(error) => {
          tracing::warn!(%error, "the shell has no call support");
          hybrid
        },
      }
    } else {
      // Media runs in the desktop shell; the browser has no calls yet.
      hybrid
    };
    return Ok(Rc::new(hybrid));
  }
  let chat = RestChat::load(auth.as_ref(), me, now_ms()).await?;
  Ok(Rc::new(Hybrid::with_live(chat, me.role.clone(), auth)))
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
        provide_context(crate::groups::GroupsHub::new(crate::rest::RestClient::new(control.authenticator())));
        provide_context(crate::channel_policy::ChannelPolicyHub::new(crate::rest::RestClient::new(control.authenticator())));
        provide_context(crate::profiles::ProfileHub::new(crate::rest::RestClient::new(control.authenticator())));
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
fn EnrollScreen(
  name: String,
  has_existing_identity: bool,
  was_data_lost: bool,
  control: SessionControl,
) -> impl IntoView {
  let i18n = use_i18n();
  let is_linking = RwSignal::new(false);
  let name = StoredValue::new(name);
  let passphrase = RwSignal::new(String::new());
  let confirm = RwSignal::new(String::new());
  let device_name = RwSignal::new(default_device_name(i18n));
  let is_revealed = RwSignal::new(false);
  let input_type = move || if is_revealed.get() { "text" } else { "password" };
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

  let device_name_for_link = device_name;
  view! {
    <Show when=move || is_linking.get() fallback=|| ()>
      <LinkScreen device_name=device_name_for_link.get_untracked() control=control on_back=Callback::new(move |()| is_linking.set(false)) />
    </Show>
    <Show when=move || !is_linking.get() fallback=|| ()>
    <AuthFrame>
      <h1 translate="no">{move || i18n.t_with("enroll-title", &[("name", &name.get_value())])}</h1>
      {was_data_lost.then(|| view! {
        <div class="auth-alert" role="alert">
          <Icon name=IconName::Alert />
          <p>{move || i18n.t("enroll-data-lost")}</p>
        </div>
      })}
      <p class="muted-text">
        {move || if has_existing_identity { i18n.t("enroll-recover-subtitle") } else { i18n.t("enroll-new-subtitle") }}
      </p>
      <form class="auth-form" on:submit=submit>
        <div class="field">
          <label for="enroll-passphrase">
            {move || if has_existing_identity { i18n.t("enroll-passphrase-recover") } else { i18n.t("enroll-passphrase-new") }}
          </label>
          <div class="input-with-action">
          <input
            id="enroll-passphrase"
            class="input"
            type=input_type
            name="passphrase"
            autocomplete=if has_existing_identity { "current-password" } else { "new-password" }
            data-autofocus
            aria-describedby="enroll-passphrase-hint"
            prop:value=move || passphrase.get()
            on:input=move |ev| passphrase.set(event_target_value(&ev))
          />
          <button
            type="button"
            class="btn btn-small"
            aria-pressed=move || is_revealed.get().to_string()
            aria-controls="enroll-passphrase enroll-confirm"
            on:click=move |_| is_revealed.update(|value| *value = !*value)
          >
            {move || if is_revealed.get() { i18n.t("passphrase-hide") } else { i18n.t("passphrase-show") }}
          </button>
          </div>
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
              type=input_type
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
          <Show when=move || has_existing_identity fallback=|| ()>
            <button type="button" class="btn" on:click=move |_| is_linking.set(true)>{move || i18n.t("enroll-link")}</button>
          </Show>
          <button type="button" class="btn" on:click=move |_| control.sign_out()>{move || i18n.t("menu-log-out")}</button>
        </div>
      </form>
    </AuthFrame>
    </Show>
  }
}

/// New device of an existing account: shows a code (QR), waits for an existing device to answer,
/// shows the short code to compare, then receives the keys (e2ee.md §5).
#[component]
fn LinkScreen(device_name: String, control: SessionControl, on_back: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  let status = RwSignal::new(LinkStatus::Idle);
  let device_name = StoredValue::new(device_name);

  spawn_local(async move {
    if let Err(message) = remote_backend::link_start(&device_name.get_value()).await {
      status.set(LinkStatus::Failed { message });
    }
  });
  use_interval(LINK_POLL_MS, move || {
    spawn_local(async move {
      if matches!(status.get_untracked(), LinkStatus::Failed { .. }) {
        return;
      }
      if let Some(user) = control.enrolling_user()
        && let Ok(CoreStatus::Ready { .. }) = remote_backend::core_status(user).await
      {
        control.finish_link();
        return;
      }
      if let Ok(latest) = remote_backend::link_status().await {
        status.set(latest);
      }
    });
  });
  let back = move |_| {
    spawn_local(async { drop(remote_backend::link_cancel().await) });
    on_back.run(());
  };

  view! {
    <AuthFrame>
      <h1>{move || i18n.t("link-new-title")}</h1>
      {move || match status.get() {
        LinkStatus::Idle => view! { <p class="auth-status" role="status"><span class="spinner" aria-hidden="true"></span></p> }.into_any(),
        LinkStatus::WaitingForScan { code } | LinkStatus::Confirm { code, .. } => {
          let short = match status.get() {
            LinkStatus::Confirm { short_code, .. } => Some(short_code),
            _ => None,
          };
          view! {
            <p class="muted-text">{move || i18n.t("link-new-steps")}</p>
            <div class="link-qr"><QrImage data=code.clone() /></div>
            <p class="small mono link-code" translate="no">{code}</p>
            {match short {
              Some(short_code) => {
                view! {
                  <p>{move || i18n.t("link-new-compare")}</p>
                  <p class="approval-code" translate="no">{short_code}</p>
                }
                .into_any()
              },
              None => view! { <p class="status-line" role="status">{move || i18n.t("link-waiting")}</p> }.into_any(),
            }}
          }
          .into_any()
        },
        LinkStatus::Failed { message } => {
          view! {
            <p class="field-error" role="alert">{move || i18n.t("link-new-failed")}</p>
            <p class="small mono" translate="no">{message}</p>
          }
          .into_any()
        },
      }}
      <div class="auth-actions">
        <button type="button" class="btn" on:click=back>{move || i18n.t("link-new-back")}</button>
      </div>
    </AuthFrame>
  }
}

const LINK_POLL_MS: u64 = 1000;

/// Short passphrases are cheap to guess offline; the minimum is a UI rule, the cost of the key
/// derivation (Argon2id) does the rest.
const MIN_PASSPHRASE_CHARS: usize = 12;

/// "Desktop (Linux x86_64)" in the desktop app, "Browser (Linux x86_64)" in a browser: the name
/// the user sees later in the device list, so it should say what this device is.
fn default_device_name(i18n: crate::i18n::I18n) -> String {
  let platform = web_sys::window()
    .and_then(|window| window.navigator().platform().ok())
    .unwrap_or_default();
  let kind = if tauri::is_available() {
    i18n.t("device-kind-desktop")
  } else {
    i18n.t("device-kind-browser")
  };
  if platform.is_empty() {
    kind
  } else {
    format!("{kind} ({platform})")
  }
}

/// Sign-in for local accounts: guests who have no account at the identity provider.
#[component]
fn LocalSignIn(control: SessionControl) -> impl IntoView {
  let i18n = use_i18n();
  let is_open = RwSignal::new(false);
  let login = RwSignal::new(String::new());
  let password = RwSignal::new(String::new());
  let code = RwSignal::new(String::new());
  let submit = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    let totp = code.get_untracked();
    control.sign_in_local(
      login.get_untracked().trim().to_owned(),
      password.get_untracked(),
      (!totp.trim().is_empty()).then(|| totp.trim().to_owned()),
    );
  };
  view! {
    <Show
      when=move || is_open.get()
      fallback=move || view! { <button class="link-btn" on:click=move |_| is_open.set(true)>{move || i18n.t("login-local-open")}</button> }
    >
      <form class="auth-local" on:submit=submit>
        <h2>{move || i18n.t("login-local-title")}</h2>
        <label for="local-login">{move || i18n.t("login-local-email")}</label>
        <input id="local-login" class="input" type="email" autocomplete="username" required data-autofocus prop:value=move || login.get() on:input=move |ev| login.set(event_target_value(&ev)) />
        <label for="local-password">{move || i18n.t("login-local-password")}</label>
        <input id="local-password" class="input" type="password" autocomplete="current-password" required prop:value=move || password.get() on:input=move |ev| password.set(event_target_value(&ev)) />
        <Show when=move || control.local_error.get() == Some(LocalSignInProblem::CodeRequired) fallback=|| ()>
          <label for="local-code">{move || i18n.t("login-local-code")}</label>
          <input id="local-code" class="input" inputmode="numeric" autocomplete="one-time-code" maxlength="8" required prop:value=move || code.get() on:input=move |ev| code.set(event_target_value(&ev)) />
          <p class="muted-text small">{move || i18n.t("login-local-code-hint")}</p>
        </Show>
        {move || {
          match control.local_error.get() {
            Some(LocalSignInProblem::Refused) => Some(i18n.t("login-local-refused")),
            Some(LocalSignInProblem::Failed(detail)) => Some(format!("{} ({detail})", i18n.t("login-local-failed"))),
            _ => None,
          }
          .map(|message| view! { <p class="field-error" role="alert">{message}</p> })
        }}
        <button type="submit" class="btn btn-primary" prop:disabled=move || control.is_local_signing_in.get()>
          {move || i18n.t("login-local-submit")}
        </button>
      </form>
    </Show>
  }
}
