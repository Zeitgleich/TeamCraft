//! The window manager's own window menu (restore, move, resize, close, "always on top") on a
//! right-click on the custom title bar (#174), as a native title bar shows it.
//!
//! GTK opens it with `gdk_window_show_window_menu`, which needs the button press that asks for it
//! (X11 and Wayland both use its device, position and serial). The press arrives in the webview
//! before the page tells the shell about its `contextmenu`, so the webview keeps the latest
//! right-button press here and the command uses it.

use std::cell::RefCell;

use gtk::gdk;
use gtk::glib::Propagation;
use gtk::prelude::WidgetExt;

/// The secondary mouse button.
const SECONDARY_BUTTON: u32 = 3;

thread_local! {
  /// The latest right-button press in the webview. GTK objects live on the main thread only, and so
  /// do the webview's handler and [`show`], which `window_chrome` runs there.
  static LAST_SECONDARY_PRESS: RefCell<Option<gdk::Event>> = const { RefCell::new(None) };
}

/// Remembers every right-button press of `webview`, for [`show`]. Called on the main thread.
pub fn watch(webview: &webkit2gtk::WebView) {
  WidgetExt::connect_button_press_event(webview, |_, press| {
    if press.button() == SECONDARY_BUTTON {
      let event: gdk::Event = (**press).clone();
      LAST_SECONDARY_PRESS.with(|last| *last.borrow_mut() = Some(event));
    }
    // The page still gets the press: its `contextmenu` is what asks for the menu.
    Propagation::Proceed
  });
}

/// Opens the window menu for the press [`watch`] kept. Returns whether the window manager took it.
/// Call on the main thread.
pub fn show(window: &gtk::ApplicationWindow) -> bool {
  let Some(mut event) = LAST_SECONDARY_PRESS.with(|last| last.borrow_mut().take()) else {
    return false;
  };
  window
    .window()
    .is_some_and(|gdk_window| gdk_window.show_window_menu(&mut event))
}
