//! The brand of the server (docs/branding.md §5): name, logo, colors, login text and links of the
//! host name the app talks to. It is applied before sign-in, cached per server URL so the next start
//! paints at once, and refreshed in the background.
//!
//! The server's answer is checked by `teamcraft_client_api::brand` (hex colors only, asset paths of the
//! brand endpoint only, https links only) before anything is applied. Colors are set through the CSSOM
//! (`style.setProperty`), which the content security policy allows; the web build also links the
//! server's `theme.css`, so the first paint already has them.

use std::cell::RefCell;
use std::collections::HashMap;

use base64::Engine;
use leptos::prelude::*;
use serde::Deserialize;
use teamcraft_client_api::brand::{BrandView, cache_key};
use teamcraft_proto::BrandResponse;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::auth::{AppConfig, fetch, tauri};
use crate::clock::now_ms;
use crate::model::TimestampMs;
use crate::reactive::{SetIfAlive, UpdateIfAlive};
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

/// Brand images of the current server the desktop shell fetched, by asset path. The desktop
/// webview's content security policy loads no images from the server; the paths are the same on
/// every server, so the cache is emptied when the user switches servers.
#[derive(Clone, Copy)]
struct ShellImages(RwSignal<HashMap<String, ShellImage>>);

#[derive(Clone, Debug, PartialEq, Eq)]
enum ShellImage {
  Loading,
  /// A `data:` URL.
  Ready(String),
  /// Failed at this time (milliseconds); tried again after [`IMAGE_RETRY_MS`].
  Failed(TimestampMs),
}

/// How long a failed brand image (offline, server error) waits before it is fetched again.
const IMAGE_RETRY_MS: TimestampMs = 30_000;

/// What an `<img>` loads for a brand asset URL of [`BrandView`]: the URL itself in the browser
/// (same origin), the `data:` URL from the desktop shell once [`request_image`] fetched it.
/// `None` while there is nothing to show. Reactive, without side effects. (Development builds
/// with a `teamcraft.server_url` override get absolute URLs of that server; the browser loads
/// them as they are.)
pub fn image_src(url: &str) -> Option<String> {
  if !tauri::is_available() {
    return Some(url.to_owned());
  }
  let images = use_context::<ShellImages>()?;
  images.0.with(|map| match map.get(url) {
    Some(ShellImage::Ready(data_url)) => Some(data_url.clone()),
    _ => None,
  })
}

/// Desktop: starts fetching a brand image through the shell unless it is loaded, loading, or
/// failed less than [`IMAGE_RETRY_MS`] ago. Call from an effect, not while rendering.
pub fn request_image(url: &str) {
  if !tauri::is_available() {
    return;
  }
  let Some(images) = use_context::<ShellImages>() else {
    return;
  };
  let now = now_ms();
  let is_wanted = images.0.with_untracked(|map| match map.get(url) {
    None => true,
    Some(ShellImage::Failed(at)) => now - *at >= IMAGE_RETRY_MS,
    Some(ShellImage::Loading | ShellImage::Ready(_)) => false,
  });
  if !is_wanted {
    return;
  }
  images.0.update(|map| {
    map.insert(url.to_owned(), ShellImage::Loading);
  });
  let path = url.to_owned();
  leptos::task::spawn_local(async move {
    let entry = match load_shell_image(&path).await {
      Some(data_url) => ShellImage::Ready(data_url),
      None => {
        tracing::debug!(path = %path, "could not load a brand image");
        ShellImage::Failed(now_ms())
      },
    };
    images.0.update_if_alive(|map| {
      // Emptied meanwhile (another server): this answer belongs to the old one.
      if map.get(&path) == Some(&ShellImage::Loading) {
        map.insert(path, entry);
      }
    });
  });
}

/// A brand image through the desktop shell (`public_asset`), as a `data:` URL.
async fn load_shell_image(path: &str) -> Option<String> {
  #[derive(Deserialize)]
  struct Payload {
    status: u16,
    content_type: Option<String>,
    body_base64: String,
  }
  let payload = tauri::call::<Payload>("public_asset", &serde_json::json!({ "path": path }))
    .await
    .ok()?;
  let content_type = payload.content_type.filter(|_| payload.status == 200)?;
  Some(format!("data:{content_type};base64,{}", payload.body_base64))
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
  provide_context(ShellImages(RwSignal::new(HashMap::new())));

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
          brand.0.set_if_alive(view);
        },
        None => tracing::debug!("the server's brand is unusable; keeping the current one"),
      },
      Err(error) => tracing::debug!(%error, "could not load the brand"),
    }
  });
}

/// Desktop: the user confirmed a server; show its brand (from its `server-info` answer) at once
/// and keep it for the next start.
pub fn show_server_brand(response: BrandResponse) {
  let config = AppConfig::load();
  let Some(view) = BrandView::from_response(response.clone(), &config.server_url) else {
    return;
  };
  if let Ok(text) = serde_json::to_string(&response) {
    store_string(&cache_key(&config.server_url), &text);
  }
  // Another server: its images have the same paths as the previous server's.
  if let Some(images) = use_context::<ShellImages>() {
    images.0.set(HashMap::new());
  }
  Brand::use_current().0.set(view);
}

/// Desktop, no server chosen (the server entry): the product's own look. The brand of a server the
/// user left is forgotten too, so it does not come back on the next start before a server is
/// chosen. Only the confirmation and what follows show a server's brand.
pub fn show_product_brand() {
  let config = AppConfig::load();
  // An empty value reads as missing (`load_string`).
  store_string(&cache_key(&config.server_url), "");
  if let Some(images) = use_context::<ShellImages>() {
    images.0.set(HashMap::new());
  }
  let brand = Brand::use_current().0;
  if !brand.with_untracked(BrandView::is_fallback) {
    brand.set(BrandView::fallback());
  }
}

/// The product mark (assets/brand/mark.svg) as a `data:` URL: shown while no server brand is
/// known, without a request.
pub fn product_icon() -> String {
  const MARK: &str = include_str!("../icons/mark.svg");
  format!(
    "data:image/svg+xml;base64,{}",
    base64::engine::general_purpose::STANDARD.encode(MARK)
  )
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
  document.set_title(&view.page_title());
}

/// The tab's icon: always the product's mark, so people find TeamCraft among their tabs whatever
/// the brand, with a red dot while something is unread (as the sidebar's red badges count it).
pub fn show_unread_in_favicon(has_unread: bool) {
  let Some(document) = web_sys::window().and_then(|window| window.document()) else {
    return;
  };
  let href = if has_unread {
    "/icons/favicon-unread.svg"
  } else {
    "/icons/favicon.svg"
  };
  set_favicon(&document, href);
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
    drop(link.set_attribute("type", "image/svg+xml"));
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
  let handler = Closure::<dyn Fn()>::new(move || signal.set_if_alive(system_prefers_dark()));
  if query
    .add_event_listener_with_callback("change", handler.as_ref().unchecked_ref())
    .is_ok()
  {
    // The listener lives as long as the page.
    handler.forget();
  }
}
