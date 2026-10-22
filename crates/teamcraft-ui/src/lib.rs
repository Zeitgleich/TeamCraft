//! TeamCraft UI: the Leptos application shared by the desktop (Tauri) and web builds.
//!
//! The UI talks to the rest of the client only through [`backend::ChatBackend`]; this crate currently
//! ships a mock implementation ([`mock::MockBackend`]) so the shell can be developed and reviewed
//! without a server.

pub mod admin;
pub mod backend;
pub mod calls;
pub mod clock;
pub mod components;
pub mod i18n;
pub mod markdown;
pub mod mock;
pub mod model;
pub mod reactive;
pub mod sections;
pub mod security;
pub mod settings;
pub mod state;

pub use components::app::App;

/// Mounts the application into `<body>`. Called from the wasm entry point.
pub fn mount() {
  console_error_panic_hook::set_once();
  leptos::mount::mount_to_body(App);
}
