//! Links from outside the app (docs/app-links.md).
//!
//! Desktop: the shell collects what the operating system hands it (`teamcraft://`) and says when
//! there is something new. A link of the server in use becomes a [`Route`] that waits here until the
//! signed-in app takes it (`history.rs`), whatever screen it arrives on; any other link asks first,
//! naming the host. Nothing happens by itself: a link names a place.
//!
//! Browser: the "Open in the app" button. The page can't know whether an app is installed, and on
//! most instances the system won't route `https://` links to one (the app would have to list the
//! host when it is built), so it offers the scheme form and leaves the choice to the reader.

use leptos::prelude::*;
use teamcraft_client_api::app_link::{AppLink, AppLinkStep, AppLinkTarget, next_step, parse_app_link};
use teamcraft_client_api::remote::DEEP_LINK_EVENT;
use teamcraft_client_api::server_entry::{HttpPolicy, ServerOrigin};

use crate::auth::tauri;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::reactive::SetIfAlive;
use crate::route::Route;

/// `localStorage`: the reader closed the button once, so it stays away.
const BUTTON_DISMISSED_KEY: &str = "teamcraft.open-in-app-dismissed";

/// Where links from outside wait for the app to be ready for them.
#[derive(Clone, Copy)]
pub struct AppLinks {
  /// A place on the server in use, until the signed-in app shows it.
  pub maybe_place: RwSignal<Option<Route>>,
  /// A host the user chose to sign in to, until the session checks it.
  pub maybe_server_to_check: RwSignal<Option<String>>,
  maybe_question: RwSignal<Option<Question>>,
}

/// What a link makes the app ask. No `Debug`: the web address can hold a share key or a one-time
/// token in its fragment.
#[derive(Clone, PartialEq, Eq)]
enum Question {
  /// Another instance. Offers signing in there only when that is possible.
  Foreign {
    host: String,
    registrable_domain: String,
    can_add_account: bool,
    /// Shown after signing in, on the new account.
    maybe_place: Option<Route>,
    web_url: String,
  },
  /// A link only the web client can open.
  WebOnly {
    host: String,
    registrable_domain: String,
    web_url: String,
  },
}

impl AppLinks {
  /// The context the app's screens reach this through.
  pub fn provide() -> Self {
    let links = Self {
      maybe_place: RwSignal::new(None),
      maybe_server_to_check: RwSignal::new(None),
      maybe_question: RwSignal::new(None),
    };
    provide_context(links);
    links
  }

  /// Desktop: takes links from the shell, those the app was started with and every later one.
  pub fn listen(self) {
    if !tauri::is_available() {
      return;
    }
    tauri::listen_text(DEEP_LINK_EVENT, move |_| self.take_from_shell());
    self.take_from_shell();
  }

  fn take_from_shell(self) {
    leptos::task::spawn_local(async move {
      match tauri::call::<Vec<String>>("deep_link_take", &serde_json::json!({})).await {
        Ok(links) => {
          for link in links {
            self.receive(&link).await;
          }
        },
        Err(error) => tracing::warn!(%error, "could not take the links from the shell"),
      }
    });
  }

  async fn receive(self, text: &str) {
    let link = match parse_app_link(text) {
      Ok(link) => link,
      // Never the link itself: it can hold a key (security.md).
      Err(error) => {
        tracing::info!(%error, "ignored a link from outside the app");
        return;
      },
    };
    // Unknown server state: the strictest answer, browser only.
    let (maybe_active, is_locked) = match tauri::server_state().await {
      Ok(state) => (
        active_origin(state.maybe_active.map(|view| view.origin)),
        state.is_locked,
      ),
      Err(_) => (None, true),
    };
    match next_step(&link, maybe_active.as_ref(), is_locked) {
      AppLinkStep::Navigate => self.maybe_place.set_if_alive(place_of(&link)),
      AppLinkStep::AskForeignServer {
        host,
        registrable_domain,
        can_add_account,
      } => {
        self.ask(link.web_url(), |web_url| Question::Foreign {
          host,
          registrable_domain,
          can_add_account,
          maybe_place: place_of(&link),
          web_url,
        });
      },
      AppLinkStep::AskWebOnly {
        host,
        registrable_domain,
      } => {
        self.ask(link.web_url(), |web_url| Question::WebOnly {
          host,
          registrable_domain,
          web_url,
        });
      },
    }
  }

  /// Never swaps a question the user is reading: what they answer must be what they read. A link
  /// arriving meanwhile is dropped; a real one can be opened again.
  fn ask(self, maybe_web_url: Option<String>, question: impl FnOnce(String) -> Question) {
    if self.maybe_question.try_with_untracked(Option::is_some) != Some(false) {
      return;
    }
    if let Some(web_url) = maybe_web_url {
      self.maybe_question.set_if_alive(Some(question(web_url)));
    }
  }
}

fn active_origin(maybe_origin: Option<String>) -> Option<ServerOrigin> {
  ServerOrigin::parse(&maybe_origin?, HttpPolicy::Allowed).ok()
}

fn place_of(link: &AppLink) -> Option<Route> {
  Route::parse(&link.path())
}

/// The question about a link, above every screen.
#[component]
pub fn AppLinkPrompt(links: AppLinks) -> impl IntoView {
  let i18n = use_i18n();
  // Above the sign-in too, where `Modal` isn't available: the focus goes to the harmless choice.
  let cancel_button = NodeRef::<leptos::html::Button>::new();
  Effect::new(move |_| {
    if let Some(button) = cancel_button.get() {
      drop(button.focus());
    }
  });
  let close = move || links.maybe_question.set(None);
  let open_in_browser = move |url: String| {
    close();
    crate::external_links::open_in_system(url);
  };
  move || {
    let question = links.maybe_question.get()?;
    let (text_key, host, domain, maybe_sign_in, web_url) = match question {
      Question::Foreign {
        host,
        registrable_domain,
        can_add_account: true,
        maybe_place,
        web_url,
      } => (
        "app-link-foreign-add",
        host.clone(),
        registrable_domain,
        Some((host, maybe_place)),
        web_url,
      ),
      Question::Foreign {
        host,
        registrable_domain,
        web_url,
        ..
      } => ("app-link-foreign-other", host, registrable_domain, None, web_url),
      Question::WebOnly {
        host,
        registrable_domain,
        web_url,
      } => ("app-link-web-only", host, registrable_domain, None, web_url),
    };
    let sign_in_button = maybe_sign_in.map(|(host, maybe_place)| {
      let on_sign_in = move |_| {
        close();
        links.maybe_place.set(maybe_place);
        links.maybe_server_to_check.set(Some(host.clone()));
      };
      view! {
        <button type="button" class="btn btn-primary" on:click=on_sign_in>
          {move || i18n.t("app-link-sign-in")}
        </button>
      }
    });
    Some(view! {
      <div class="modal-backdrop" on:keydown=move |event: web_sys::KeyboardEvent| {
        if event.key() == "Escape" {
          close();
        }
      }>
        <div class="modal modal-small" role="dialog" aria-modal="true" aria-labelledby="app-link-title">
          <header class="modal-head">
            <h2 id="app-link-title">{move || i18n.t("app-link-title")}</h2>
          </header>
          <div class="modal-body">
            <p>{move || i18n.t(text_key)}</p>
            <p class="app-link-host" translate="no" dir="ltr">
              <strong class="app-link-domain">{domain}</strong>
              <span class="app-link-full-host">{host}</span>
            </p>
            <footer class="modal-foot">
              <span class="grow"></span>
              <button type="button" class="btn" node_ref=cancel_button on:click=move |_| close()>
                {move || i18n.t("cancel")}
              </button>
              <button type="button" class="btn" on:click=move |_| open_in_browser(web_url.clone())>
                {move || i18n.t("app-link-open-browser")}
              </button>
              {sign_in_button}
            </footer>
          </div>
        </div>
      </div>
    })
  }
}

/// Browser: "Open in the app" on a page that is a place in the app. Never redirects by itself, since
/// without the app the link would lead nowhere. In the page's flow (the banner row, the sign-in card),
/// never above it: nothing fixed may cover the composer's send or a call's controls.
#[component]
pub fn OpenInAppButton() -> impl IntoView {
  let i18n = use_i18n();
  let maybe_link = button_link();
  let is_shown = RwSignal::new(maybe_link.is_some());
  let dismiss = move |_| {
    is_shown.set(false);
    crate::settings::store_string(BUTTON_DISMISSED_KEY, "1");
  };
  move || {
    let link = maybe_link.clone().filter(|_| is_shown.get())?;
    Some(view! {
      <div class="update-banner open-in-app" role="region" aria-label=move || i18n.t("open-in-app")>
        <a class="btn btn-small btn-primary" href=link on:click=move |_| is_shown.set(false)>
          {move || i18n.t("open-in-app")}
        </a>
        <span class="grow"></span>
        <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=dismiss>
          <Icon name=IconName::Close />
        </button>
      </div>
    })
  }
}

/// The scheme form of this page's address, if the button belongs here.
fn button_link() -> Option<String> {
  if tauri::is_available()
    || crate::settings::load_string(BUTTON_DISMISSED_KEY).is_some()
    || crate::components::edge_swipe::is_installed()
  {
    return None;
  }
  let location = web_sys::window()?.location();
  let address = format!(
    "{}{}{}",
    location.origin().ok()?,
    location.pathname().ok()?,
    location.hash().ok()?
  );
  button_link_of(&address)
}

/// The sign-in page and the bare address aren't a place worth leaving the browser for.
fn button_link_of(address: &str) -> Option<String> {
  let link = parse_app_link(address).ok()?;
  if matches!(link.target(), AppLinkTarget::Start) {
    return None;
  }
  link.scheme_link()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_button_offers_places_and_keeps_secrets_out_of_the_scheme() {
    assert_eq!(
      button_link_of("https://chat.example.com/c/2/m/3").as_deref(),
      Some("teamcraft://open/chat.example.com/c/2/m/3")
    );
    for address in [
      "https://chat.example.com/",
      "https://chat.example.com/settings",
      "https://chat.example.com/f/abc#key",
      "https://chat.example.com/meeting/123#invite=abc",
    ] {
      assert_eq!(button_link_of(address), None, "{address}");
    }
  }
}
