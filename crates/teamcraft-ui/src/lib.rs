//! TeamCraft UI: the Leptos application shared by the desktop (Tauri) and web builds.
//!
//! The UI talks to the rest of the client only through [`backend::ChatBackend`]; this crate currently
//! ships a mock implementation ([`mock::MockBackend`]) so the shell can be developed and reviewed
//! without a server.

// The backend traits and view models live in `teamcraft-client-api`; the old paths keep working.
pub use teamcraft_client_api::{admin, backend, model, search, security};

pub mod admin_live;
pub mod auth;
pub mod calls;
pub mod clock;
pub mod components;
pub mod hybrid;
pub mod i18n;
pub mod live;
pub mod markdown;
pub mod meetings;
pub mod mock;
pub mod notifier;
pub mod notify_sync;
pub mod prefs;
pub mod reactive;
pub mod remote_backend;
pub mod sections;
pub mod server_backend;
pub mod session;
pub mod settings;
pub mod state;

pub use components::app::App;

/// Mounts the application into `<body>`. Called from the wasm entry point.
pub fn mount() {
  console_error_panic_hook::set_once();
  leptos::mount::mount_to_body(App);
}
