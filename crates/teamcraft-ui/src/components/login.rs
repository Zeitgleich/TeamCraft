//! Screens around sign-in: checking the session, the login screen with its problems, the welcome
//! screen of a new device, and the gate that decides which one to show.

use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::brand::BrandView;
use teamcraft_client_api::remote::StoreProblem;
use teamcraft_client_api::sign_in_page::SignInPage;
use teamcraft_proto::{GetMeResponse, SignInMethod};
use wasm_bindgen::JsCast;

use crate::auth::SignInLook;
use crate::auth::tauri::{self, ServerCheck, ServerProblem};
use crate::auth::{self, AppConfig};
use crate::backend::Backend;
use crate::brand::{Brand, image_src};
use crate::clock::now_ms;
use crate::components::app::Shell;
use crate::components::brand_logo::BrandLogo;
use crate::components::bundle_banner::BundleBanner;
use crate::components::icons::{Icon, IconName};
use crate::components::pending_button::PendingButton;
use crate::components::security_ui::QrImage;
use crate::feedback::PendingAction;
use crate::hybrid::Hybrid;
use crate::i18n::use_i18n;
use crate::meetings::{MeetingsClient, MeetingsHub};
use crate::notify_sync::NotificationSync;
use crate::reactive::{SetIfAlive, use_interval};
use crate::remote_backend::{self, RemoteChat};
use crate::remote_calls::RemoteCalls;
use crate::server_backend::{LoadError, RestChat};
use crate::session::{LocalSignInProblem, LoginProblem, SessionControl, SessionState};
use crate::settings::Settings;
use crate::sign_in_page::{SignInBlock, SignInPageState, blocks};
use teamcraft_client_api::remote::{CoreStatus, LinkStatus};
use teamcraft_client_api::sign_in_look::LookTheme;

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
      SessionState::OpeningCore => view! { <OpeningCoreScreen control=control /> }.into_any(),
      SessionState::NeedsEnrollment { me, has_existing_identity, was_data_lost } => {
        view! { <EnrollScreen name=me.display_name has_existing_identity=has_existing_identity was_data_lost=was_data_lost control=control /> }.into_any()
      },
      SessionState::NeedsDeviceSetup(me) => view! { <WelcomeScreen name=me.display_name control=control /> }.into_any(),
      SessionState::Ready(me) => view! { <DataGate me=me settings=settings control=control /> }.into_any(),
    }}
  }
}

/// The card every sign-in screen sits on: the brand of the server on top (unless the screen shows
/// a brand itself, like the server confirmation), the screen, the encryption note and the brand's
/// links at the bottom.
#[component]
pub(crate) fn AuthFrame(
  /// The screen shows a brand of its own; the current one would be the wrong one.
  #[prop(optional)]
  hide_brand: bool,
  children: Children,
) -> impl IntoView {
  let i18n = use_i18n();
  let brand = Brand::use_current().0;
  let language = move || i18n.locale().tag();
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
  // Effects run after the first render, when the screen's controls exist.
  Effect::new(move |_| focus_autofocus());
  view! {
    <main class="auth-page">
      <BundleBanner />
      <section class="auth-card">
        {(!hide_brand).then(|| view! {
          <div class="auth-brand" translate="no">
            {move || brand_mark(brand)}
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
        })}
        <div class="auth-content">{children()}</div>
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

/// The top of the card: no server brand yet → the product mark and wordmark; a logo (a
/// wordmark) → the logo alone; an icon → the icon with the name beside it; nothing → the name.
fn brand_mark(brand: RwSignal<BrandView>) -> AnyView {
  let name = move || brand.with(|view| view.display_name.clone());
  let (is_product, has_wordmark) = brand.with(|view| (view.is_fallback(), view.has_wordmark()));
  if is_product {
    // Each wordmark carries the name: only the theme's one is displayed (and so read), and it
    // still reads "TeamCraft" if the image fails.
    return view! {
      <img class="auth-logo-img auth-product-icon" src=crate::brand::product_icon() alt="" />
      <img class="auth-product-wordmark brand-light" src="/icons/wordmark.svg" alt=name />
      <img class="auth-product-wordmark brand-dark" src="/icons/wordmark-dark.svg" alt=name />
    }
    .into_any();
  }
  if has_wordmark {
    // Until the logo is there, or when it fails, the name stands in for it.
    return view! {
      <BrandLogo class="auth-logo-img" fallback=move || view! { <span class="auth-brand-name">{name}</span> } />
    }
    .into_any();
  }
  view! {
    <BrandLogo class="auth-logo-img auth-icon-img" fallback=|| () />
    <span class="auth-brand-name">{name}</span>
  }
  .into_any()
}

#[component]
fn CheckingScreen() -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <AuthFrame>
      <p class="auth-status auth-status-quiet" role="status">
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
      <div class="auth-actions">
        <button class="btn auth-wide" data-autofocus on:click=move |_| control.cancel_sign_in()>
          {move || i18n.t("cancel")}
        </button>
      </div>
    </AuthFrame>
  }
}

/// Signed in; the encrypted data opens as soon as the core has the access token.
#[component]
fn OpeningCoreScreen(control: SessionControl) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <AuthFrame>
      <h1>{move || i18n.t("login-opening-core-title")}</h1>
      <p class="auth-status" role="status">
        <span class="spinner" aria-hidden="true"></span>
        <span>{move || i18n.t("login-opening-core")}</span>
      </p>
      <div class="auth-actions">
        <button class="btn auth-wide" on:click=move |_| control.sign_out()>
          {move || i18n.t("login-core-sign-out")}
        </button>
      </div>
    </AuthFrame>
  }
}

/// How a problem card looks: an error, or a notice when nothing went wrong (a cancelled sign-in).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProblemTone {
  Error,
  Notice,
}

/// A problem in plain language, with the technical detail (when there is one) folded away under
/// "Technical details", for the admins.
#[component]
fn ProblemCard(
  maybe_title_key: Option<String>,
  body_key: String,
  maybe_detail: Option<String>,
  #[prop(default = ProblemTone::Error)] tone: ProblemTone,
) -> impl IntoView {
  let i18n = use_i18n();
  let maybe_detail = maybe_detail.filter(|detail| !detail.trim().is_empty());
  view! {
    <div class="auth-problem" class:auth-problem-notice=tone == ProblemTone::Notice role="alert">
      <Icon name=IconName::Alert />
      <div class="auth-problem-text">
        {maybe_title_key.map(|key| view! { <strong class="auth-problem-title">{move || i18n.t(&key)}</strong> })}
        <p>{move || i18n.t(&body_key)}</p>
        {maybe_detail.map(|detail| {
          view! {
            <details class="auth-details">
              <summary>{move || i18n.t("login-details")}</summary>
              <p class="mono" translate="no">{detail}</p>
            </details>
          }
        })}
      </div>
    </div>
  }
}

#[component]
fn LoginScreen(problem: Option<LoginProblem>, control: SessionControl) -> impl IntoView {
  let i18n = use_i18n();
  control.local_error.set(None);
  let server_url = control
    .server
    .get_untracked()
    .map_or_else(|| AppConfig::load().server_url, |server| server.origin);
  let sign_in = SignInPageState::load(server_url);
  let form = LocalForm::new();
  let is_role_problem = problem == Some(LoginProblem::NoTeamcraftRole);
  let is_core_problem = matches!(
    problem,
    Some(LoginProblem::CoreFailed(_) | LoginProblem::CoreStopped | LoginProblem::CoreNotSignedIn)
  );
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
    Some(LoginProblem::CoreStopped) => (Some("login-core-stopped-title"), Some("login-core-stopped-body")),
    Some(LoginProblem::StoreUnreadable(StoreProblem::NewerVersion)) => {
      (Some("login-store-newer-title"), Some("login-store-newer-body"))
    },
    Some(LoginProblem::StoreUnreadable(_)) => (
      Some("login-store-damaged-title"),
      Some("login-store-damaged-body"),
    ),
    Some(LoginProblem::CoreNotSignedIn) => (
      Some("login-core-no-token-title"),
      Some("login-core-no-token-body"),
    ),
    None => (None, None),
  };
  let tone = if problem == Some(LoginProblem::Cancelled) {
    ProblemTone::Notice
  } else {
    ProblemTone::Error
  };
  let subtitle = move || i18n.t(sign_in.page.with(subtitle_key));

  view! {
    <AuthFrame>
      <h1>{move || i18n.t("login-title")}</h1>
      <p class="auth-subtitle muted-text">{subtitle}</p>
      {title_key
        .zip(body_key)
        .map(|(title, body)| {
          view! {
            <ProblemCard
              maybe_title_key=Some(title.to_owned())
              body_key=body.to_owned()
              maybe_detail=detail
              tone=tone
            />
          }
        })}
      {if is_role_problem {
        view! {
          <div class="auth-actions">
            <button class="btn btn-primary auth-wide" data-autofocus on:click=move |_| control.sign_out()>
              {move || i18n.t("login-use-other-account")}
            </button>
          </div>
        }
        .into_any()
      } else if let Some(LoginProblem::StoreUnreadable(store_problem)) = problem.clone() {
        view! { <StoreProblemActions problem=store_problem control=control /> }.into_any()
      } else if is_core_problem {
        // Still signed in: retry the core, or sign out (which also unlocks changing the server).
        view! {
          <div class="auth-actions auth-actions-split">
            <button class="btn btn-primary" data-autofocus on:click=move |_| control.retry_core()>
              {move || i18n.t("login-core-retry")}
            </button>
            <button class="btn" on:click=move |_| control.sign_out()>
              {move || i18n.t("login-core-sign-out")}
            </button>
          </div>
        }
        .into_any()
      } else {
        view! { <SignInMethods state=sign_in control=control form=form /> }.into_any()
      }}
      <SignInHelp page=sign_in.page />
      <ServerLine control=control />
      {demo_switch()}
    </AuthFrame>
  }
}

/// The subtitle under "Sign in", by what the page offers.
fn subtitle_key(page: &SignInPage) -> &'static str {
  match (page.offers(&SignInMethod::Sso), page.offers(&SignInMethod::Local)) {
    (true, true) => "login-subtitle-choice",
    (false, true) => "login-subtitle-local",
    _ => "login-subtitle",
  }
}

/// The ways to sign in, in the server's order, with "or" between them. The first one's control has
/// the focus.
#[component]
fn SignInMethods(state: SignInPageState, control: SessionControl, form: LocalForm) -> impl IntoView {
  let i18n = use_i18n();
  let is_desktop = tauri::is_available();
  let parts = Memo::new(move |_| state.page.with(|page| blocks(page, is_desktop)));
  Effect::new(move |_| {
    parts.track();
    focus_autofocus();
  });
  view! {
    <div class="auth-methods" aria-busy=move || state.is_loading.get().to_string()>
      {move || {
        parts
          .get()
          .into_iter()
          .enumerate()
          .map(|(index, part)| {
            let is_first = index == 0;
            match part {
              SignInBlock::Sso { label, maybe_icon_url } => {
                view! { <SsoButton label=label maybe_icon_url=maybe_icon_url is_first=is_first control=control page=state.page /> }.into_any()
              },
              SignInBlock::Local { requires_code } => {
                view! { <LocalSignInForm requires_code=requires_code is_first=is_first control=control form=form /> }.into_any()
              },
              SignInBlock::Divider => {
                view! {
                  <p class="auth-divider">
                    <span>{move || i18n.t("login-or")}</span>
                  </p>
                }
                .into_any()
              },
            }
          })
          .collect_view()
      }}
    </div>
  }
}

/// "Sign in with {provider}": the identity provider, in the system browser on desktop.
#[component]
fn SsoButton(
  label: String,
  maybe_icon_url: Option<String>,
  is_first: bool,
  control: SessionControl,
  /// The server's sign-in page, for whether it asks for the provider's login page.
  page: Memo<SignInPage>,
) -> impl IntoView {
  let i18n = use_i18n();
  let text = move || i18n.t_with("login-sso", &[("provider", &label)]);
  let brand = Brand::use_current().0;
  // The brand's images as the card shows them (desktop: `data:` URLs from the shell).
  let logos = Memo::new(move |_| {
    brand.with(|view| {
      let maybe_light = view.header_image(false).and_then(image_src);
      let maybe_dark = view
        .header_image(true)
        .filter(|dark| Some(*dark) != view.header_image(false))
        .and_then(image_src);
      (
        view.display_name.clone(),
        maybe_light,
        maybe_dark,
        view.has_wordmark(),
      )
    })
  });
  let start = move |_| {
    let (name, maybe_logo, maybe_logo_dark, is_wordmark) = logos.get_untracked();
    let mut look = sign_in_look(i18n, Some(name), maybe_logo, maybe_logo_dark, is_wordmark);
    look.maybe_login_page_always_shown = page.with_untracked(|page| page.maybe_login_page_always_shown);
    control.sign_in(look);
  };
  view! {
    <button
      type="button"
      class="btn btn-primary auth-sso"
      data-autofocus=is_first
      on:click=start
    >
      {maybe_icon_url.map(|url| {
        view! {
          <img
            class="auth-sso-icon"
            src=url
            alt=""
            on:error=|ev| {
              // A missing icon leaves the label alone instead of a broken image.
              if let Some(image) = ev.target().and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok()) {
                drop(image.style().set_property("display", "none"));
              }
            }
          />
        }
      })}
      <span>{text}</span>
    </button>
  }
}

/// What the user typed into the local sign-in form; owned by the screen, so a reload of the
/// server's page settings keeps it.
#[derive(Clone, Copy)]
struct LocalForm {
  login: RwSignal<String>,
  password: RwSignal<String>,
  code: RwSignal<String>,
  is_revealed: RwSignal<bool>,
}

impl LocalForm {
  fn new() -> Self {
    Self {
      login: RwSignal::new(String::new()),
      password: RwSignal::new(String::new()),
      code: RwSignal::new(String::new()),
      is_revealed: RwSignal::new(false),
    }
  }
}

/// Sign-in with e-mail, password and the one-time code of the authenticator app. The code field is
/// always there: the server answers a missing code like a wrong password, so the form can't ask
/// for it afterwards.
#[component]
fn LocalSignInForm(
  requires_code: bool,
  is_first: bool,
  control: SessionControl,
  form: LocalForm,
) -> impl IntoView {
  let i18n = use_i18n();
  let is_busy = move || control.is_local_signing_in.get();
  let submit = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    if control.is_local_signing_in.get_untracked() {
      return;
    }
    let code = normalize_code(&form.code.get_untracked());
    control.sign_in_local(
      form.login.get_untracked().trim().to_owned(),
      form.password.get_untracked(),
      (!code.is_empty()).then_some(code),
    );
  };
  // A refused code is spent; the next attempt needs the current one.
  Effect::new(move |_| {
    if control.local_error.get() == Some(LocalSignInProblem::Refused) {
      form.code.set(String::new());
    }
  });
  let has_error = move || control.local_error.with(Option::is_some);
  let reveal_icon = Signal::derive(move || {
    if form.is_revealed.get() {
      IconName::EyeOff
    } else {
      IconName::Eye
    }
  });
  view! {
    <form class="auth-local" on:submit=submit aria-describedby=move || has_error().then_some("local-error")>
      <div class="field">
        <label for="local-login">{move || i18n.t("login-local-email")}</label>
        <input
          id="local-login"
          class="input"
          type="email"
          name="username"
          inputmode="email"
          autocomplete="username"
          autocapitalize="off"
          spellcheck="false"
          required
          data-autofocus=is_first
          prop:value=move || form.login.get()
          on:input=move |ev| form.login.set(event_target_value(&ev))
        />
      </div>
      <div class="field">
        <label for="local-password">{move || i18n.t("login-local-password")}</label>
        <div class="auth-password">
          <input
            id="local-password"
            class="input"
            type=move || if form.is_revealed.get() { "text" } else { "password" }
            name="password"
            autocomplete="current-password"
            autocapitalize="off"
            spellcheck="false"
            required
            prop:value=move || form.password.get()
            on:input=move |ev| form.password.set(event_target_value(&ev))
          />
          <button
            type="button"
            class="auth-reveal"
            aria-label=move || i18n.t("login-local-show-password")
            title=move || i18n.t("login-local-show-password")
            aria-pressed=move || form.is_revealed.get().to_string()
            aria-controls="local-password"
            on:click=move |_| form.is_revealed.update(|is_revealed| *is_revealed = !*is_revealed)
          >
            <Icon name=reveal_icon />
          </button>
        </div>
      </div>
      <div class="field">
        <label for="local-code">
          {move || if requires_code { i18n.t("login-local-code") } else { i18n.t("login-local-code-optional") }}
        </label>
        <input
          id="local-code"
          class="input auth-code"
          type="text"
          name="one-time-code"
          inputmode="numeric"
          autocomplete="one-time-code"
          autocapitalize="off"
          spellcheck="false"
          maxlength="10"
          required=requires_code
          aria-describedby="local-code-hint"
          prop:value=move || form.code.get()
          on:input=move |ev| form.code.set(event_target_value(&ev))
        />
        <p id="local-code-hint" class="muted-text small">{move || i18n.t("login-local-code-hint")}</p>
      </div>
      {move || {
        control
          .local_error
          .get()
          .map(|problem| {
            let (key, maybe_detail) = local_problem_message(&problem);
            view! {
              <p id="local-error" class="auth-error" role="alert">
                <Icon name=IconName::Alert />
                <span>
                  {move || i18n.t(key)}
                  {maybe_detail.map(|detail| view! { <span class="small mono" translate="no">{detail}</span> })}
                </span>
              </p>
            }
          })
      }}
      <button
        type="submit"
        class="btn auth-submit"
        class:btn-primary=is_first
        prop:disabled=is_busy
        aria-busy=move || is_busy().to_string()
      >
        <Show when=is_busy fallback=|| ()>
          <span class="spinner" aria-hidden="true"></span>
        </Show>
        <span>{move || if is_busy() { i18n.t("login-local-working") } else { i18n.t("login-local-submit") }}</span>
      </button>
    </form>
  }
}

/// The message of a local sign-in problem and, for unexpected failures, a details line.
fn local_problem_message(problem: &LocalSignInProblem) -> (&'static str, Option<String>) {
  match problem {
    LocalSignInProblem::Refused => ("login-local-refused", None),
    LocalSignInProblem::Disabled => ("login-local-disabled", None),
    LocalSignInProblem::RateLimited => ("login-local-rate-limited", None),
    LocalSignInProblem::Network => ("login-local-network", None),
    LocalSignInProblem::Failed(detail) => ("login-local-failed", Some(detail.clone())),
  }
}

/// "123 456" and "123-456" as authenticator apps show them: digits only.
fn normalize_code(typed: &str) -> String {
  typed
    .chars()
    .filter(|ch| !ch.is_whitespace() && *ch != '-')
    .collect()
}

/// The server's help below the form: a text and a link to its help page.
#[component]
fn SignInHelp(page: Memo<SignInPage>) -> impl IntoView {
  let i18n = use_i18n();
  move || {
    let (maybe_text, maybe_link) =
      page.with(|page| (page.maybe_help_text.clone(), page.maybe_help_link.clone()));
    (maybe_text.is_some() || maybe_link.is_some()).then(|| {
      view! {
        <div class="auth-help small">
          {maybe_text.map(|text| view! { <p class="auth-help-text">{text}</p> })}
          {maybe_link.map(|link| {
            view! {
              <a href=link target="_blank" rel="noopener noreferrer">
                {move || i18n.t("login-help-link")}
              </a>
            }
          })}
        </div>
      }
    })
  }
}

/// Focuses the control marked `data-autofocus` on the auth card, unless the user is already in a
/// control (a late reload of the page settings must not steal the focus).
fn focus_autofocus() {
  request_animation_frame(|| {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
      return;
    };
    let is_idle = document
      .active_element()
      .is_none_or(|active| active.tag_name().eq_ignore_ascii_case("body"));
    if !is_idle {
      return;
    }
    if let Some(target) = document
      .query_selector(".auth-card [data-autofocus]")
      .ok()
      .flatten()
      .and_then(|found| found.dyn_into::<web_sys::HtmlElement>().ok())
    {
      drop(target.focus());
    }
  });
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
  // No server is chosen here: the product's look, never a server's (a cached one or one the user
  // just left). The web client has no server entry; it keeps its server's brand.
  if tauri::is_available() {
    crate::brand::show_product_brand();
  }
  let checked_address = maybe_address.unwrap_or_default();
  let address = RwSignal::new(checked_address.clone());
  let is_checking = move || control.is_checking_server.get();
  let has_problem = maybe_problem.is_some();
  let is_unknown_host = maybe_problem
    .as_ref()
    .is_some_and(|problem| problem.key == tauri::UNKNOWN_HOST_KEY);
  let submit = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    if !control.is_checking_server.get_untracked() {
      control.check_server(address.get_untracked());
    }
  };
  view! {
    <AuthFrame>
      <h1>{move || i18n.t("server-entry-title")}</h1>
      <p class="auth-subtitle muted-text">{move || i18n.t("server-entry-subtitle")}</p>
      <form class="auth-form" on:submit=submit>
        <div class="field">
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
            aria-describedby="server-address-hint"
            aria-invalid=has_problem.to_string()
            prop:readonly=move || control.is_server_locked.get()
            prop:value=move || address.get()
            on:input=move |ev| address.set(event_target_value(&ev))
          />
          <p id="server-address-hint" class="field-hint">{move || i18n.t("server-entry-hint")}</p>
        </div>
        {maybe_problem.map(|problem| {
          view! { <ProblemCard maybe_title_key=None body_key=problem.key maybe_detail=Some(problem.detail) /> }
        })}
        {is_unknown_host.then(|| view! { <AlternativesLine checked_address=checked_address address=address /> })}
        <button type="submit" class="btn btn-primary auth-submit" prop:disabled=is_checking aria-busy=move || is_checking().to_string()>
          <Show when=is_checking fallback=|| ()>
            <span class="spinner" aria-hidden="true"></span>
          </Show>
          <span>{move || i18n.t("server-entry-continue")}</span>
        </button>
      </form>
    </AuthFrame>
  }
}

/// Where the search for a similar address stands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Alternatives {
  Idle,
  Looking,
  Found(String),
  NoneFound,
}

/// Under a host name that does not exist: "Check similar addresses". Nothing is looked up until
/// the user presses it; then the shell tries the same name under other common top-level domains
/// and the first one that exists is offered.
#[component]
fn AlternativesLine(checked_address: String, address: RwSignal<String>) -> impl IntoView {
  let i18n = use_i18n();
  let state = RwSignal::new(Alternatives::Idle);
  let checked_address = StoredValue::new(checked_address);
  let look = move |_| {
    state.set(Alternatives::Looking);
    let typed = checked_address.get_value();
    spawn_local(async move {
      let next = match tauri::server_suggest(&typed).await {
        Ok(Some(host)) => Alternatives::Found(host),
        Ok(None) => Alternatives::NoneFound,
        Err(problem) => {
          tracing::debug!(key = %problem.key, "could not look for similar addresses");
          Alternatives::NoneFound
        },
      };
      state.set_if_alive(next);
    });
  };
  move || match state.get() {
    Alternatives::Idle | Alternatives::Looking => {
      let is_looking = state.with(|state| *state == Alternatives::Looking);
      view! {
        <p class="auth-suggest">
          <button type="button" class="link-btn" prop:disabled=is_looking aria-busy=is_looking.to_string() on:click=look>
            {is_looking.then(|| view! { <span class="spinner" aria-hidden="true"></span> })}
            <span>{move || i18n.t("server-entry-check-alternatives")}</span>
          </button>
        </p>
      }
      .into_any()
    },
    Alternatives::Found(host) => view! { <SuggestionLine suggestion=host address=address /> }.into_any(),
    Alternatives::NoneFound => {
      view! { <p class="auth-suggest" role="status">{move || i18n.t("server-entry-no-alternative")}</p> }
        .into_any()
    },
  }
}

/// "Did you mean team.example.de?" under a host name that does not exist. Taking the hint only
/// fills in the field; the user still continues on their own.
#[component]
fn SuggestionLine(suggestion: String, address: RwSignal<String>) -> impl IntoView {
  let i18n = use_i18n();
  let host = StoredValue::new(suggestion);
  let is_taken = move || host.with_value(|host| address.with(|typed| typed.trim() == host));
  let take = move |_| {
    address.set(host.get_value());
    focus_element("server-address");
  };
  move || {
    (!is_taken()).then(|| {
      let text = host.with_value(|host| i18n.t_with("server-entry-did-you-mean", &[("host", host)]));
      let (before, after) = host
        .with_value(|host| split_around(&text, host).map(|(before, after)| (before.to_owned(), after.to_owned())))
        .unwrap_or_else(|| (text.clone(), String::new()));
      view! {
        <p class="auth-suggest" role="status">
          <Icon name=IconName::Help />
          <span>
            {before}
            <button type="button" class="link-btn mono" translate="no" on:click=take>{host.get_value()}</button>
            {after}
          </span>
        </p>
      }
    })
  }
}

/// Desktop: the server answered; its brand, host and identity provider are confirmed before the
/// browser opens.
#[component]
fn ServerConfirmScreen(check: ServerCheck, control: SessionControl) -> impl IntoView {
  let i18n = use_i18n();
  let name = check.server.display_name.clone();
  let host = check.server.host.clone();
  let provider = check.identity_provider_host.clone();
  let is_elsewhere = check.is_identity_provider_elsewhere;
  let maybe_logo = check.maybe_logo.clone().filter(|url| is_data_image(url));
  let maybe_logo_dark = check
    .maybe_logo_dark
    .clone()
    .filter(|url| is_data_image(url) && Some(url) != maybe_logo.as_ref());
  // A logo is a wordmark and stands for the name; an icon gets the name below it.
  let is_wordmark =
    BrandView::from_response(check.brand.clone(), "").is_some_and(|brand| brand.has_wordmark());
  let has_logo = maybe_logo.is_some();
  let logo_failed = RwSignal::new(false);
  let hide_on_error = move |_| logo_failed.set(true);
  let shows_name = move || !(is_wordmark && has_logo && !logo_failed.get());
  let logo_alt = if is_wordmark { name.clone() } else { String::new() };
  let look_logos = (maybe_logo.clone(), maybe_logo_dark.clone());
  let look_name = name.clone();
  let name = StoredValue::new(name);
  let check = StoredValue::new(check);
  let confirm = move |_| {
    let look = sign_in_look(
      i18n,
      Some(look_name.clone()),
      look_logos.0.clone(),
      look_logos.1.clone(),
      is_wordmark,
    );
    control.confirm_server(check.get_value(), look);
  };
  view! {
    <AuthFrame hide_brand=true>
      <div class="auth-identity" translate="no">
        <Show when=move || !logo_failed.get() fallback=|| ()>
          {maybe_logo.clone().map(|light| {
            view! {
              <img class="auth-identity-logo brand-light" class:auth-identity-icon=!is_wordmark src=light alt=logo_alt.clone() on:error=hide_on_error />
              {maybe_logo_dark.clone().map(|dark| view! {
                <img class="auth-identity-logo brand-dark" class:auth-identity-icon=!is_wordmark src=dark alt="" aria-hidden="true" on:error=hide_on_error />
              })}
            }
          })}
        </Show>
        <Show when=shows_name fallback=|| ()>
          <p class="auth-identity-name">{name.get_value()}</p>
        </Show>
        <p class="auth-identity-host mono">{host}</p>
      </div>
      <h1 class="auth-center">{move || i18n.t("server-entry-confirm-title")}</h1>
      <p class="auth-subtitle muted-text auth-center">{move || i18n.t("server-entry-confirm-body")}</p>
      <ProviderRow provider=provider.clone() />
      {is_elsewhere
        .then(|| {
          view! {
            <div class="auth-alert" role="alert">
              <Icon name=IconName::Alert />
              <p>{move || i18n.t_with("server-entry-provider-elsewhere", &[("host", &provider)])}</p>
            </div>
          }
        })}
      <div class="auth-actions auth-actions-split">
        <button
          class="btn btn-primary"
          data-autofocus
          prop:disabled=move || control.is_checking_server.get()
          aria-busy=move || control.is_checking_server.get().to_string()
          on:click=confirm
        >
          <Show when=move || control.is_checking_server.get() fallback=|| ()>
            <span class="spinner" aria-hidden="true"></span>
          </Show>
          <span>{move || i18n.t("server-entry-confirm")}</span>
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

/// "You will sign in at sso.example.com." – where the password goes (#222 H1), so it reads as body
/// text in its own row, the host set off in mono.
#[component]
fn ProviderRow(provider: String) -> impl IntoView {
  let i18n = use_i18n();
  let provider = StoredValue::new(provider);
  view! {
    <div class="auth-provider">
      <Icon name=IconName::Key />
      <p translate="no">
        {move || {
          let text = provider.with_value(|host| i18n.t_with("server-entry-provider", &[("host", host)]));
          let host = provider.get_value();
          match split_around(&text, &host) {
            Some((before, after)) => {
              let (before, after) = (before.to_owned(), after.to_owned());
              view! { {before}<strong class="mono">{host}</strong>{after} }.into_any()
            },
            None => text.into_view().into_any(),
          }
        }}
      </p>
    </div>
  }
}

/// The parts of `text` before and after the first `needle`, to set a placeable off in markup.
fn split_around<'a>(text: &'a str, needle: &str) -> Option<(&'a str, &'a str)> {
  if needle.is_empty() {
    return None;
  }
  let start = text.find(needle)?;
  Some((&text[..start], &text[start + needle.len()..]))
}

/// A `data:` image URL as the shell hands brand images over; anything else is not shown.
fn is_data_image(url: &str) -> bool {
  url.starts_with("data:image/")
}

/// What the browser's page after the sign-in looks like: the app's language and theme, the
/// server's name and logo.
fn sign_in_look(
  i18n: crate::i18n::I18n,
  maybe_brand_name: Option<String>,
  maybe_logo: Option<String>,
  maybe_logo_dark: Option<String>,
  is_wordmark: bool,
) -> SignInLook {
  SignInLook {
    maybe_locale: Some(i18n.locale().tag().to_owned()),
    theme: document_theme(),
    maybe_brand_name,
    maybe_logo: maybe_logo.filter(|url| is_data_image(url)),
    maybe_logo_dark: maybe_logo_dark.filter(|url| is_data_image(url)),
    is_wordmark,
    maybe_login_page_always_shown: None,
  }
}

/// The theme the app applied to `<html data-theme>` (none: follow the system).
fn document_theme() -> LookTheme {
  let maybe_theme = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.document_element())
    .and_then(|root| root.get_attribute("data-theme"));
  match maybe_theme.as_deref() {
    Some("light") => LookTheme::Light,
    Some("dark") => LookTheme::Dark,
    _ => LookTheme::System,
  }
}

/// Moves the focus to the element with this id (after the view updated).
fn focus_element(id: &'static str) {
  request_animation_frame(move || {
    if let Some(element) = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|document| document.get_element_by_id(id))
      .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    {
      drop(element.focus());
    }
  });
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
      browser_calls(hybrid)
    };
    return Ok(Rc::new(hybrid));
  }
  let chat = RestChat::load(auth.as_ref(), me, now_ms()).await?;
  Ok(Rc::new(Hybrid::with_live(chat, me.role.clone(), auth)))
}

/// The browser runs call media in the page (#330), where the browser can and this tab runs the core.
#[cfg(target_arch = "wasm32")]
fn browser_calls<C: crate::backend::ChatBackend + 'static>(hybrid: Hybrid<C>) -> Hybrid<C> {
  match crate::web_calls::support() {
    Ok(()) => hybrid.with_calls(RemoteCalls::in_page(crate::web_calls::WebCallHost::new())),
    Err(reason) => {
      tracing::info!(?reason, "no calls in this tab");
      hybrid.without_calls(reason)
    },
  }
}

#[cfg(not(target_arch = "wasm32"))]
fn browser_calls<C: crate::backend::ChatBackend + 'static>(hybrid: Hybrid<C>) -> Hybrid<C> {
  hybrid
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
      backend.set_if_alive(Some(load_backend(auth, &me).await));
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
        provide_context(crate::space_images::SpaceImageHub::new(control.authenticator()));
        view! { <Shell backend=loaded settings=settings /> }.into_any()
      },
      Some(Err(error)) => {
        tracing::warn!(%error, "could not load the account data");
        let detail = error.to_string();
        view! {
          <AuthFrame>
            <ProblemCard
              maybe_title_key=Some("data-failed-title".to_owned())
              body_key="data-failed-body".to_owned()
              maybe_detail=Some(detail)
            />
            <div class="auth-actions auth-actions-split">
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
      status.set_if_alive(LinkStatus::Failed { message });
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
        status.set_if_alive(latest);
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

/// What the user can do about a local database this version can't read (issue #160). Setting the
/// device up again needs a confirmation: the browser deletes the data for good. Data from a newer
/// version is only ever reset on the desktop: in the browser a newer tab or bundle can read it, so
/// reloading is the way out, and an old cached page must not wipe it.
#[component]
fn StoreProblemActions(problem: StoreProblem, control: SessionControl) -> impl IntoView {
  let i18n = use_i18n();
  let is_desktop = tauri::is_available();
  let is_newer = problem == StoreProblem::NewerVersion;
  let can_reset = is_desktop || !is_newer;
  let is_confirming = RwSignal::new(false);
  let pending = PendingAction::new();
  let confirm_key = if is_desktop {
    "login-store-reset-confirm-desktop"
  } else {
    "login-store-reset-confirm-web"
  };
  let try_again = move |_| {
    if is_newer && !is_desktop {
      crate::bundle::reload();
    } else {
      control.retry_core();
    }
  };
  let try_again_key = if is_newer && !is_desktop {
    "login-store-reload"
  } else {
    "login-core-retry"
  };
  view! {
    <Show
      when=move || is_confirming.get()
      fallback=move || {
        view! {
          <div class="auth-actions auth-actions-split">
            {if is_newer {
              view! {
                <button class="btn btn-primary" data-autofocus on:click=try_again>
                  {move || i18n.t(try_again_key)}
                </button>
              }
                .into_any()
            } else {
              view! {
                <button class="btn btn-primary" data-autofocus on:click=move |_| is_confirming.set(true)>
                  {move || i18n.t("login-store-reset")}
                </button>
              }
                .into_any()
            }}
            {(is_newer && can_reset)
              .then(|| {
                view! {
                  <button class="btn" on:click=move |_| is_confirming.set(true)>
                    {move || i18n.t("login-store-reset")}
                  </button>
                }
              })}
            {(!is_newer)
              .then(|| {
                view! {
                  <button class="btn" on:click=try_again>
                    {move || i18n.t(try_again_key)}
                  </button>
                }
              })}
            <button class="btn" on:click=move |_| control.sign_out()>
              {move || i18n.t("login-core-sign-out")}
            </button>
          </div>
        }
      }
    >
      <p class="auth-confirm-text" role="alert">{move || i18n.t(confirm_key)}</p>
      <div class="auth-actions auth-actions-split">
        <PendingButton
          pending=pending
          class="btn btn-danger"
          on_click=move |_| {
            pending
              .spawn(async move {
                control.reset_store().await;
                Ok(())
              })
          }
        >
          {move || i18n.t("login-store-reset-confirm-button")}
        </PendingButton>
        <button
          class="btn"
          data-autofocus
          prop:disabled=move || pending.is_pending()
          on:click=move |_| is_confirming.set(false)
        >
          {move || i18n.t("login-store-reset-cancel")}
        </button>
      </div>
    </Show>
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_subtitle_follows_the_offered_methods() {
    let mut page = SignInPage::sso_only("Example Chat");
    assert_eq!(subtitle_key(&page), "login-subtitle");
    page.methods = vec![SignInMethod::Local];
    assert_eq!(subtitle_key(&page), "login-subtitle-local");
    page.methods = vec![SignInMethod::Local, SignInMethod::Sso];
    assert_eq!(subtitle_key(&page), "login-subtitle-choice");
  }

  #[test]
  fn local_problems_have_one_message_each_and_details_only_for_failures() {
    assert_eq!(
      local_problem_message(&LocalSignInProblem::Refused),
      ("login-local-refused", None)
    );
    assert_eq!(
      local_problem_message(&LocalSignInProblem::Disabled),
      ("login-local-disabled", None)
    );
    assert_eq!(
      local_problem_message(&LocalSignInProblem::RateLimited),
      ("login-local-rate-limited", None)
    );
    assert_eq!(
      local_problem_message(&LocalSignInProblem::Network),
      ("login-local-network", None)
    );
    assert_eq!(
      local_problem_message(&LocalSignInProblem::Failed("internal".into())),
      ("login-local-failed", Some("internal".to_owned()))
    );
  }

  #[test]
  fn a_placeable_is_split_out_of_its_sentence() {
    assert_eq!(
      split_around("You will sign in at sso.example.com.", "sso.example.com"),
      Some(("You will sign in at ", "."))
    );
    assert_eq!(
      split_around("Meintest du chat.example.de?", "chat.example.de"),
      Some(("Meintest du ", "?"))
    );
    assert_eq!(split_around("no host here", "sso.example.com"), None);
    assert_eq!(split_around("text", ""), None);
  }

  #[test]
  fn only_data_images_are_shown_or_passed_on() {
    assert!(is_data_image("data:image/png;base64,AA=="));
    assert!(!is_data_image("https://example.com/logo.png"));
    assert!(!is_data_image("data:text/html;base64,AA=="));
    assert!(!is_data_image("javascript:alert(1)"));
  }

  #[test]
  fn codes_are_sent_as_digits_only() {
    assert_eq!(normalize_code(" 123 456 "), "123456");
    assert_eq!(normalize_code("123-456"), "123456");
    assert_eq!(normalize_code("   "), "");
  }
}
