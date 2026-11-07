//! The composers wait for a typing pause before they store a draft. When the main window goes to
//! the background or closes, the shell asks them to store it now, or the last words typed are lost
//! (#1220). A close that quits the app waits a moment for the drafts to reach the core.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{Emitter, Window};
use teamcraft_client_api::remote::PERSIST_DRAFTS_SHELL_EVENT;

use crate::tray::MAIN_WINDOW;

/// How long a quitting close waits for the composers' drafts: the event, their write through the
/// core's command queue. Short enough not to feel like a hang.
const CLOSE_GRACE: Duration = Duration::from_millis(300);

/// Set once the drafts were asked for before a quitting close, so the close that follows goes through.
static IS_CLOSE_FLUSHED: AtomicBool = AtomicBool::new(false);

/// Asks the main window's composers to store a waiting draft now.
pub fn persist(window: &Window) {
  if let Err(error) = window.emit_to(MAIN_WINDOW, PERSIST_DRAFTS_SHELL_EVENT, ()) {
    tracing::debug!(%error, "could not ask the composers for their drafts");
  }
}

/// A close of the main window that quits the app: asks for the drafts first and closes after
/// [`CLOSE_GRACE`]. Returns whether the close is to be held back now.
pub fn hold_close(window: &Window) -> bool {
  if IS_CLOSE_FLUSHED.swap(false, Ordering::SeqCst) {
    return false;
  }
  persist(window);
  let window = window.clone();
  tauri::async_runtime::spawn(async move {
    tokio::time::sleep(CLOSE_GRACE).await;
    IS_CLOSE_FLUSHED.store(true, Ordering::SeqCst);
    if let Err(error) = window.close() {
      tracing::debug!(%error, "could not close the window after storing the drafts");
    }
  });
  true
}
