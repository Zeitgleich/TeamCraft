//! Mouse wheel scrolling like the rest of the desktop (#806). WebKitGTK animates every wheel notch
//! with its own short scroll animation, which other Linux apps don't: a notch feels slow and
//! sluggish next to them. Worse, the message list corrects its position by script while rows above
//! the top one get measured (scrolling up), and a scripted scroll ends WebKit's animation early, so
//! part of each notch is lost. Without the animation a notch moves by its full step at once.
//! Touchpad scrolling sends precise deltas, which WebKitGTK never animated; its kinetic scrolling
//! after the fingers lift stays.

use webkit2gtk::{SettingsExt, WebViewExt};

/// Turns WebKitGTK's per-notch smooth scrolling off for `webview`.
pub fn configure(webview: &webkit2gtk::WebView) {
  if let Some(settings) = WebViewExt::settings(webview) {
    settings.set_enable_smooth_scrolling(false);
  }
}
