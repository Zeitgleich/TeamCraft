//! The brand of the server (docs/branding.md §5): name, logo, colors, login text and links of the
//! host name the app talks to. It is applied before sign-in, cached per server URL so the next start
//! paints at once, and refreshed in the background.
//!
//! The server's answer is checked by `teamcraft_client_api::brand` (hex colors only, asset paths of the
//! brand endpoint only, https links only) before anything is applied. Colors are set through the CSSOM
//! (`style.setProperty`), which the content security policy allows; the web build also links the
//! server's `theme.css`, so the first paint already has them.

use std::cell::RefCell;

use leptos::prelude::*;
use serde::Deserialize;
use teamcraft_client_api::brand::{BrandView, cache_key};
use teamcraft_proto::BrandResponse;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::auth::{AppConfig, fetch, tauri};
use crate::settings::{Settings, Theme, load_string, store_string};

const BRAND_PATH: &str = "/api/v1/brand";

/// The brand being shown, readable everywhere below [`provide_brand`].
#[derive(Clone, Copy)]
pub struct Brand(pub RwSignal<BrandView>);

impl Brand {
  /// The brand signal; the fallback brand if none was provided (tests, mock mode before load).
  pub fn use_current() -> Self {
    use_context::<Self>().unwrap_or_else(|| Self(RwSignal::new(BrandView::fallback())))
  }
}

/// Loads the cached brand of the configured server, shows it, and refreshes it from the server.
pub fn provide_brand(settings: Settings) {
  let config = AppConfig::load();
  let key = cache_key(&config.server_url);
  let cached = load_string(&key)
    .and_then(|text| serde_json::from_str::<BrandResponse>(&text).ok())
    .and_then(|response| BrandView::from_response(response, &config.server_url));
  let brand = Brand(RwSignal::new(cached.unwrap_or_else(BrandView::fallback)));
  provide_context(brand);

  let system_dark = RwSignal::new(system_prefers_dark());
  watch_system_scheme(system_dark);
  Effect::new(move |_| {
    let is_dark = match settings.theme.get() {
      Theme::Dark => true,
      Theme::Light => false,
      Theme::System => system_dark.get(),
    };
    apply_to_document(&brand.0.get(), is_dark);
  });

  leptos::task::spawn_local(async move {
    match fetch_brand(&config).await {
      Ok(text) => match serde_json::from_str::<BrandResponse>(&text)
        .ok()
        .and_then(|response| BrandView::from_response(response, &config.server_url))
      {
        Some(view) => {
          store_string(&key, &text);
          brand.0.set(view);
        },
        None => tracing::debug!("the server's brand is unusable; keeping the current one"),
      },
      Err(error) => tracing::debug!(%error, "could not load the brand"),
    }
  });
}

/// `GET /api/v1/brand` without credentials: through the desktop shell, or the page's own fetch.
async fn fetch_brand(config: &AppConfig) -> Result<String, String> {
  #[derive(Deserialize)]
  struct Payload {
    status: u16,
    body: String,
  }
  let payload = if tauri::is_available() {
    tauri::call::<Payload>("public_get", &serde_json::json!({ "path": BRAND_PATH })).await?
  } else {
    let response = fetch::fetch(
      "GET",
      &config.api_url(BRAND_PATH),
      &[("Accept".to_owned(), "application/json".to_owned())],
      None,
    )
    .await?;
    Payload {
      status: response.status,
      body: String::from_utf8_lossy(&response.body).into_owned(),
    }
  };
  if payload.status == 200 {
    Ok(payload.body)
  } else {
    Err(format!("the server answered {}", payload.status))
  }
}

thread_local! {
  /// Custom properties set by the last [`apply_to_document`], so a theme switch removes stale ones.
  static APPLIED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Sets the brand's colors on `<html>`, the page title and the favicon.
fn apply_to_document(view: &BrandView, is_dark: bool) {
  let Some(document) = web_sys::window().and_then(|window| window.document()) else {
    return;
  };
  if let Some(root) = document
    .document_element()
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
  {
    let style = root.style();
    APPLIED.with(|applied| {
      for property in applied.borrow_mut().drain(..) {
        drop(style.remove_property(&property));
      }
      for (property, value) in view.properties(is_dark) {
        if style.set_property(property, value).is_ok() {
          applied.borrow_mut().push(property.clone());
        }
      }
    });
  }
  document.set_title(&view.display_name);
  if let Some(icon) = view
    .asset("favicon", is_dark)
    .or_else(|| view.asset("icon", is_dark))
  {
    set_favicon(&document, icon);
  }
}

fn set_favicon(document: &web_sys::Document, href: &str) {
  let link = document
    .query_selector("link[rel~='icon']")
    .ok()
    .flatten()
    .or_else(|| {
      let link = document.create_element("link").ok()?;
      link.set_attribute("rel", "icon").ok()?;
      document.head()?.append_child(&link).ok()?;
      Some(link)
    });
  if let Some(link) = link {
    drop(link.set_attribute("href", href));
  }
}

fn system_prefers_dark() -> bool {
  web_sys::window()
    .and_then(|window| window.match_media("(prefers-color-scheme: dark)").ok().flatten())
    .is_some_and(|query| query.matches())
}

/// Keeps `signal` equal to the system's dark preference.
fn watch_system_scheme(signal: RwSignal<bool>) {
  let Some(query) =
    web_sys::window().and_then(|window| window.match_media("(prefers-color-scheme: dark)").ok().flatten())
  else {
    return;
  };
  let handler = Closure::<dyn Fn()>::new(move || signal.set(system_prefers_dark()));
  if query
    .add_event_listener_with_callback("change", handler.as_ref().unchecked_ref())
    .is_ok()
  {
    // The listener lives as long as the page.
    handler.forget();
  }
}
