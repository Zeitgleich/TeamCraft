//! One running instance of the app on every desktop OS (e2ee.md §9.4).
//!
//! A second process would open the same encrypted store with its own copy in memory, and the
//! device's MLS state and outbox would fork. A second launch therefore hands its arguments to the
//! running instance (Windows: named mutex and window message, Linux: D-Bus, macOS: a Unix socket),
//! which shows its window, and exits. Where that fails (Linux without a session bus) the store's
//! file lock (`teamcraft_core::store::StoreLock`) keeps the second process off the store.

use tauri::Wry;
use tauri::plugin::TauriPlugin;
use tracing::debug;

use crate::tray;

/// The plugin; registered before every other one, so a second launch ends before it does anything.
pub fn plugin() -> TauriPlugin<Wry> {
  tauri_plugin_single_instance::init(|app, args, _working_directory| {
    // On Windows the deep-link plugin has already consumed any link in `args` (`deep-link` feature).
    debug!(
      argument_count = args.len(),
      "second instance started; showing the first"
    );
    tray::show_main_window(app);
  })
}
