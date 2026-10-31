//! Mouse wheel scrolling like the rest of the desktop (#806). WebKitGTK animates every wheel notch
//! with its own scroll animation (the step is the same either way): a notch lands about 60 ms late,
//! and a quick series of notches trails behind the wheel by a few hundred milliseconds, which feels
//! sluggish next to other Linux apps. Without the animation each notch moves at once.
//! Touchpad scrolling has precise deltas, which WebKit never animates, and its kinetic scrolling
//! after the fingers lift doesn't depend on this setting.

use webkit2gtk::{SettingsExt, WebViewExt};

/// Turns WebKitGTK's per-notch smooth scrolling off for `webview`.
pub fn configure(webview: &webkit2gtk::WebView) {
  if let Some(settings) = WebViewExt::settings(webview) {
    settings.set_enable_smooth_scrolling(false);
  }
}
