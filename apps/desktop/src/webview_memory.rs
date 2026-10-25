//! WebKitGTK defaults for an app that stays open all day (docs/performance.md, "Desktop on Linux:
//! memory"). WebKitGTK starts as a web browser: it keeps pages for back and forward and up to 64 MiB
//! of resources no page shows any more (decoded pictures among them) in case they come back. The
//! app is one page that never navigates, and the pictures it shows are `blob:` URLs that are revoked
//! when they go, so neither cache is ever hit; they only hold memory.

use webkit2gtk::{CacheModel, SettingsExt, WebContextExt, WebViewExt};

/// Sets the memory caches of the webview's process to what a single-page app uses.
pub fn configure(webview: &webkit2gtk::WebView) {
  // Keeps no dead resources and no back/forward pages; live resources are capped as before.
  if let Some(context) = webview.context() {
    context.set_cache_model(CacheModel::DocumentViewer);
  }
  // wry turns it on; it keeps the whole page when navigating away, which the app never does.
  if let Some(settings) = WebViewExt::settings(webview) {
    settings.set_enable_page_cache(false);
  }
}
