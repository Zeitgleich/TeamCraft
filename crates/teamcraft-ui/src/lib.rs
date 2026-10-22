//! TeamCraft UI: the Leptos application shared by the desktop (Tauri) and web builds.
//!
//! The UI talks to the rest of the client only through [`backend::ChatBackend`]; this crate currently
//! ships a mock implementation ([`mock::MockBackend`]) so the shell can be developed and reviewed
//! without a server.

// The backend traits and view models live in `teamcraft-client-api`; the old paths keep working.
pub use teamcraft_client_api::{admin, backend, model, search, security};

pub mod admin_live;
pub mod auth;
pub mod bundle;
pub mod calls;
pub mod clock;
pub mod components;
pub mod emoji;
pub mod highlight;
pub mod hybrid;
pub mod i18n;
pub mod live;
pub mod markdown;
pub mod meetings;
pub mod mock;
pub mod notifier;
pub mod notify_sync;
pub mod prefs;
pub mod profiles;
pub mod reactive;
pub mod remote_backend;
pub mod remote_calls;
pub mod rest;
pub mod sections;
pub mod server_backend;
pub mod session;
pub mod settings;
pub mod state;
#[cfg(target_arch = "wasm32")]
pub mod web_core;

pub use components::app::App;

/// Mounts the application into `<body>`. Called from the wasm entry point.
pub fn mount() {
  console_error_panic_hook::set_once();
  register_service_worker();
  leptos::mount::mount_to_body(App);
}

/// Browser only: the service worker makes the app installable and wakes it for pushes. The desktop
/// shell has no service worker (and no use for one).
fn register_service_worker() {
  if auth::tauri::is_available() {
    return;
  }
  let Some(window) = web_sys::window() else { return };
  let container = window.navigator().service_worker();
  let promise = container.register("/sw.js");
  wasm_bindgen_futures::spawn_local(async move {
    if let Err(error) = wasm_bindgen_futures::JsFuture::from(promise).await {
      // Plain http outside localhost has no service workers; the app works without.
      tracing::debug!(?error, "no service worker");
    }
  });
}
