//! What the service worker says about the app code that is running (docs/e2ee.md §9.3): signed by
//! the release key, not signed (development), or an update that did not verify. The worker posts
//! `{type: "bundle", state, version, key_fingerprint, reason}`; the pages ask once with `status`.

use leptos::prelude::*;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

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

#[cfg(test)]
mod tests {
  use super::*;

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
