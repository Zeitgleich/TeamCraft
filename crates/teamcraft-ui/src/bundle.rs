//! What the service worker says about the app code that is running (docs/e2ee.md §9.3): signed by
//! the release key, not signed (development), or an update that did not verify. The worker posts
//! `{type: "bundle", state, version, key_fingerprint, reason}`; the pages ask once with `status`.
//!
//! Updates: a new release waits in the service worker until the user reloads into it (or every tab
//! closed); the page offers that reload. The page asks the browser to look for a new release now and
//! then, and asks the server whether this version is still supported.

use std::cell::Cell;
use std::time::Duration;

use crate::reactive::SetIfAlive;
use leptos::prelude::*;
use teamcraft_client_api::updates::ClientVersion;
use teamcraft_proto::ClientCompatibilityResponse;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

/// At most this often the page looks for a new release (`registration.update()`) and asks the
/// server which versions it supports.
const UPDATE_CHECK_THROTTLE: Duration = Duration::from_secs(15 * 60);
/// A page open for long looks this often even without becoming visible or online again.
const UPDATE_CHECK_INTERVAL_MS: u64 = 60 * 60 * 1000;
/// If the new release does not take over within this time after "reload", reload anyway.
const SKIP_WAITING_TIMEOUT: Duration = Duration::from_secs(5);
const COMPATIBILITY_PATH: &str = "/api/v1/client-compatibility";

thread_local! {
  static LAST_UPDATE_CHECK_MS: Cell<f64> = const { Cell::new(f64::MIN) };
  static IS_RELOADING: Cell<bool> = const { Cell::new(false) };
}

/// Verification state of the web bundle.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BundleState {
  /// No answer yet (no worker, the desktop shell, or still installing).
  #[default]
  Unknown,
  /// Every file matched the signed release manifest.
  Verified {
    version: String,
    key_fingerprint: String,
  },
  /// The worker has no release key pinned: a development build.
  Unsigned,
  /// The browser cannot verify Ed25519 signatures; the app runs unverified.
  Unsupported,
  /// The server offered a bundle that does not verify; the previous verified one keeps running.
  Rejected { reason: String },
}

impl BundleState {
  /// Reads a worker message; `None` for anything else.
  pub fn from_message(data: &JsValue) -> Option<Self> {
    let text = |key: &str| {
      js_sys::Reflect::get(data, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default()
    };
    if text("type") != "bundle" {
      return None;
    }
    Some(Self::parse(
      &text("state"),
      text("version"),
      text("key_fingerprint"),
      text("reason"),
    ))
  }

  fn parse(state: &str, version: String, key_fingerprint: String, reason: String) -> Self {
    match state {
      "verified" => Self::Verified {
        version,
        key_fingerprint,
      },
      "unsigned" => Self::Unsigned,
      "unsupported" => Self::Unsupported,
      "rejected" => Self::Rejected { reason },
      _ => Self::Unknown,
    }
  }

  /// Whether the user should be told right away.
  pub fn needs_warning(&self) -> bool {
    match self {
      Self::Rejected { .. } => true,
      // A development build is expected to be unsigned; a release build should not be.
      Self::Unsigned | Self::Unsupported => !cfg!(debug_assertions),
      Self::Unknown | Self::Verified { .. } => false,
    }
  }
}

/// The state shared through context; created once in `App`.
#[derive(Clone, Copy)]
pub struct Bundle(pub RwSignal<BundleState>);

/// Whether a newer release than this page's should take over.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ReleaseUpdate {
  #[default]
  Current,
  /// A verified new release waits, or another tab already reloaded into it: reloading switches.
  Ready,
  /// The server no longer supports this version (`/client-compatibility`); reloading picks up the
  /// release the server serves now.
  TooOld { minimum_version: String },
}

/// The update state shared through context; created once in `App`.
#[derive(Clone, Copy)]
pub struct Release(pub RwSignal<ReleaseUpdate>);

/// Follows the service worker's messages and asks for the current state. Browser only.
pub fn watch(state: RwSignal<BundleState>) {
  let Some(window) = web_sys::window() else { return };
  let container = window.navigator().service_worker();
  let listener = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
    if let Some(next) = BundleState::from_message(&event.data()) {
      state.set(next);
    }
  });
  if container
    .add_event_listener_with_callback("message", listener.as_ref().unchecked_ref())
    .is_err()
  {
    return;
  }
  listener.forget();
  let Ok(ask) = container.ready() else { return };
  wasm_bindgen_futures::spawn_local(async move {
    if let Ok(registration) = wasm_bindgen_futures::JsFuture::from(ask).await
      && let Ok(registration) = registration.dyn_into::<web_sys::ServiceWorkerRegistration>()
      && let Some(worker) = registration.active()
    {
      let request = js_sys::Object::new();
      drop(js_sys::Reflect::set(
        &request,
        &JsValue::from_str("type"),
        &JsValue::from_str("status"),
      ));
      drop(worker.post_message(&request));
    }
  });
}

/// Follows the service worker registration for a waiting release and checks for one now and then
/// (when the page becomes visible, comes back online, and hourly; throttled). Browser only.
pub fn watch_releases(state: RwSignal<ReleaseUpdate>) {
  let Some(window) = web_sys::window() else { return };
  let container = window.navigator().service_worker();
  let on_controller_change = Closure::<dyn FnMut()>::new(move || {
    if IS_RELOADING.get() {
      reload();
    } else {
      // Another tab reloaded into the new release; this page still runs the old one.
      state.set(ReleaseUpdate::Ready);
    }
  });
  if container
    .add_event_listener_with_callback("controllerchange", on_controller_change.as_ref().unchecked_ref())
    .is_ok()
  {
    // Lives as long as the page.
    on_controller_change.forget();
  }
  let Ok(ready) = container.ready() else { return };
  wasm_bindgen_futures::spawn_local(async move {
    let Ok(registration) = wasm_bindgen_futures::JsFuture::from(ready).await else {
      return;
    };
    let registration: web_sys::ServiceWorkerRegistration = registration.unchecked_into();
    if registration.waiting().is_some() && has_controller() {
      state.set_if_alive(ReleaseUpdate::Ready);
    }
    follow_installs(&registration, state);
    check_for_update(&registration, state);
    let check = {
      let registration = registration.clone();
      move || check_for_update(&registration, state)
    };
    let on_wake = Closure::<dyn FnMut()>::new({
      let check = check.clone();
      move || {
        if web_sys::window()
          .and_then(|window| window.document())
          .is_some_and(|document| document.visibility_state() == web_sys::VisibilityState::Visible)
        {
          check();
        }
      }
    });
    if let Some(window) = web_sys::window() {
      for event in ["visibilitychange", "online"] {
        drop(window.add_event_listener_with_callback(event, on_wake.as_ref().unchecked_ref()));
      }
    }
    on_wake.forget();
    // Only the throttle decides; checks while hidden are cheap and skipped often.
    if let Some(window) = web_sys::window() {
      let tick = Closure::<dyn FnMut()>::new(check);
      drop(window.set_interval_with_callback_and_timeout_and_arguments_0(
        tick.as_ref().unchecked_ref(),
        i32::try_from(UPDATE_CHECK_INTERVAL_MS).unwrap_or(i32::MAX),
      ));
      tick.forget();
    }
  });
}

/// Reloads into the new release: the waiting worker takes over, then the page reloads (on
/// `controllerchange`). Without a waiting worker (another tab switched already) it just reloads.
pub fn apply_update() {
  IS_RELOADING.set(true);
  let Some(window) = web_sys::window() else { return };
  let Ok(ready) = window.navigator().service_worker().ready() else {
    reload();
    return;
  };
  wasm_bindgen_futures::spawn_local(async move {
    let waiting = wasm_bindgen_futures::JsFuture::from(ready)
      .await
      .ok()
      .and_then(|registration| {
        registration
          .unchecked_into::<web_sys::ServiceWorkerRegistration>()
          .waiting()
      });
    let Some(worker) = waiting else {
      reload();
      return;
    };
    let message = js_sys::Object::new();
    drop(js_sys::Reflect::set(
      &message,
      &"type".into(),
      &"skip_waiting".into(),
    ));
    drop(worker.post_message(&message));
    // The page reloads on `controllerchange`; this is the fallback if that never comes.
    wait(SKIP_WAITING_TIMEOUT).await;
    reload();
  });
}

/// Marks the update as ready when a new release finished installing while this page runs.
fn follow_installs(registration: &web_sys::ServiceWorkerRegistration, state: RwSignal<ReleaseUpdate>) {
  let on_update_found = Closure::<dyn FnMut()>::new({
    let registration = registration.clone();
    move || {
      let Some(installing) = registration.installing() else {
        return;
      };
      let worker = installing.clone();
      let on_state_change = Closure::<dyn FnMut()>::new(move || {
        if worker.state() == web_sys::ServiceWorkerState::Installed && has_controller() {
          state.set(ReleaseUpdate::Ready);
        }
      });
      drop(
        installing.add_event_listener_with_callback("statechange", on_state_change.as_ref().unchecked_ref()),
      );
      on_state_change.forget();
    }
  });
  drop(
    registration.add_event_listener_with_callback("updatefound", on_update_found.as_ref().unchecked_ref()),
  );
  on_update_found.forget();
}

/// Asks the browser to look for a new release and the server whether this version is still
/// supported; at most once per [`UPDATE_CHECK_THROTTLE`].
fn check_for_update(registration: &web_sys::ServiceWorkerRegistration, state: RwSignal<ReleaseUpdate>) {
  let now = js_sys::Date::now();
  let throttle_ms = UPDATE_CHECK_THROTTLE.as_secs_f64() * 1000.0;
  if now - LAST_UPDATE_CHECK_MS.get() < throttle_ms {
    return;
  }
  LAST_UPDATE_CHECK_MS.set(now);
  if let Ok(promise) = registration.update() {
    wasm_bindgen_futures::spawn_local(async move {
      if let Err(error) = wasm_bindgen_futures::JsFuture::from(promise).await {
        tracing::debug!(?error, "could not look for a new release");
      }
    });
  }
  wasm_bindgen_futures::spawn_local(async move {
    match fetch_compatibility().await {
      Ok(compatibility) => {
        if let Some(update) = compatibility_update(env!("CARGO_PKG_VERSION"), &compatibility)
          && state
            .try_get_untracked()
            .is_some_and(|current| current != ReleaseUpdate::Ready)
        {
          state.set_if_alive(update);
        }
      },
      Err(error) => tracing::debug!(%error, "could not ask the server for supported versions"),
    }
  });
}

/// `TooOld` when the server needs a newer client than `running`.
fn compatibility_update(running: &str, compatibility: &ClientCompatibilityResponse) -> Option<ReleaseUpdate> {
  let running = ClientVersion::parse(running).ok()?;
  let minimum = ClientVersion::parse(&compatibility.minimum_client_version).ok()?;
  (running < minimum).then(|| ReleaseUpdate::TooOld {
    minimum_version: compatibility.minimum_client_version.clone(),
  })
}

async fn fetch_compatibility() -> Result<ClientCompatibilityResponse, String> {
  let window = web_sys::window().ok_or("no window")?;
  let response: web_sys::Response =
    wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(COMPATIBILITY_PATH))
      .await
      .map_err(|error| format!("{error:?}"))?
      .unchecked_into();
  if !response.ok() {
    return Err(format!("HTTP {}", response.status()));
  }
  let text = response.text().map_err(|error| format!("{error:?}"))?;
  let text = wasm_bindgen_futures::JsFuture::from(text)
    .await
    .map_err(|error| format!("{error:?}"))?
    .as_string()
    .unwrap_or_default();
  serde_json::from_str(&text).map_err(|error| error.to_string())
}

/// A `setTimeout` as a future.
async fn wait(duration: Duration) {
  let timeout_ms = i32::try_from(duration.as_millis()).unwrap_or(i32::MAX);
  let timer = js_sys::Promise::new(&mut |resolve, _reject| {
    if let Some(window) = web_sys::window() {
      drop(window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, timeout_ms));
    }
  });
  drop(wasm_bindgen_futures::JsFuture::from(timer).await);
}

fn has_controller() -> bool {
  web_sys::window().is_some_and(|window| window.navigator().service_worker().controller().is_some())
}

fn reload() {
  if let Some(window) = web_sys::window()
    && let Err(error) = window.location().reload()
  {
    tracing::warn!(?error, "could not reload");
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_update_banner_is_translated() {
    use crate::i18n::{Locale, translate};
    for locale in [Locale::En, Locale::De] {
      for key in ["release-ready", "release-too-old", "release-reload"] {
        assert_ne!(translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }

  #[test]
  fn only_a_server_that_needs_a_newer_client_asks_for_an_update() {
    let compatibility = |minimum: &str| ClientCompatibilityResponse {
      server_version: "1.4.0".into(),
      minimum_client_version: minimum.into(),
    };
    assert_eq!(compatibility_update("1.0.0", &compatibility("1.0.0")), None);
    assert_eq!(compatibility_update("1.2.0", &compatibility("1.0.0")), None);
    assert_eq!(
      compatibility_update("1.0.0-rc.7", &compatibility("1.0.0")),
      Some(ReleaseUpdate::TooOld {
        minimum_version: "1.0.0".into()
      })
    );
    // Garbage from the server never blocks the app.
    assert_eq!(compatibility_update("1.0.0", &compatibility("latest")), None);
  }

  #[test]
  fn states_are_read_from_the_workers_words() {
    assert_eq!(
      BundleState::parse("verified", "1.2".into(), "abcd".into(), String::new()),
      BundleState::Verified {
        version: "1.2".into(),
        key_fingerprint: "abcd".into()
      }
    );
    assert_eq!(
      BundleState::parse("unsigned", String::new(), String::new(), String::new()),
      BundleState::Unsigned
    );
    assert_eq!(
      BundleState::parse("rejected", String::new(), String::new(), "hash mismatch".into()),
      BundleState::Rejected {
        reason: "hash mismatch".into()
      }
    );
    assert_eq!(
      BundleState::parse("pending", String::new(), String::new(), String::new()),
      BundleState::Unknown
    );
  }

  #[test]
  fn only_a_rejected_bundle_always_warns() {
    assert!(
      BundleState::Rejected {
        reason: String::new()
      }
      .needs_warning()
    );
    assert!(
      !BundleState::Verified {
        version: String::new(),
        key_fingerprint: String::new()
      }
      .needs_warning()
    );
    assert!(!BundleState::Unknown.needs_warning());
  }
}
