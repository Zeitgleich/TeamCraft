//! TeamCraft UI: the Leptos application shared by the desktop (Tauri) and web builds.
//!
//! The UI talks to the rest of the client only through [`backend::ChatBackend`]. Demo data for
//! development (`mock::MockBackend`) only exists with the `demo` feature and in tests; shipped builds
//! always talk to a server.

// Demo data must never reach a user: an optimized build with it fails here.
#[cfg(all(feature = "demo", not(debug_assertions)))]
compile_error!(
  "the `demo` feature is for development builds only; release builds must not contain demo data"
);

// The backend traits and view models live in `teamcraft-client-api`; the old paths keep working.
pub use teamcraft_client_api::{admin, backend, model, search, security};

pub mod admin_live;
pub mod areas;
pub mod attention;
pub mod auth;
pub mod brand;
pub mod bundle;
pub mod call_alerts;
pub mod calls;
pub mod channel_access;
pub mod channel_policy;
pub mod clock;
pub mod components;
pub mod delivery;
pub mod diff;
pub mod editor_doc;
pub mod feed_motion;
pub mod feedback;
pub mod gestures;
// Shared with the mobile apps (through the FFI), so they live in the client API.
pub use teamcraft_client_api::{emoji, emoji_catalog};
pub mod groups;
pub mod highlight;
pub mod history;
pub mod hybrid;
pub mod i18n;
pub mod image_crop;
pub mod keys;
pub mod lightbox;
pub mod live;
pub mod markdown;
pub mod media_cache;
pub mod media_formats;
pub mod media_layout;
pub mod meeting_reminders;
pub mod meetings;
pub mod mention;
pub mod message_links;
#[cfg(any(test, feature = "demo"))]
pub mod mock;
pub mod motion;
pub mod navigation;
pub mod notifier;
pub mod notify_sync;
pub mod object_url;
pub mod os_alerts;
pub mod panic_overlay;
pub mod prefs;
pub mod profiles;
pub mod raster;
pub mod reactive;
pub mod reminders;
pub mod remote_backend;
pub mod remote_calls;
pub mod rest;
pub mod route;
pub mod sections;
pub mod server_backend;
pub mod session;
pub mod settings;
#[cfg(any(target_arch = "wasm32", test))]
pub mod shell_video;
pub mod sign_in_page;
pub mod slash;
pub mod space_images;
pub mod state;
pub mod sync_reads;
pub mod toast_motion;
pub mod toast_timer;
pub mod unread_divider;
pub mod viewport;
#[cfg(target_arch = "wasm32")]
pub mod web_calls;
#[cfg(target_arch = "wasm32")]
pub mod web_core;
#[cfg(target_arch = "wasm32")]
mod web_tabs;

pub use components::app::App;

/// Mounts the application into `<body>`. Called from the wasm entry point.
pub fn mount() {
  panic_overlay::install();
  #[cfg(feature = "demo")]
  panic_overlay::panic_on_request();
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

#[cfg(test)]
mod tests {
  /// An unclosed block swallows every rule after it without any error from the build.
  #[test]
  fn stylesheet_blocks_are_balanced() {
    let css = include_str!("../style/main.css");
    let mut depth = 0_i32;
    let mut line = 1;
    let mut in_comment = false;
    let mut chars = css.chars().peekable();
    while let Some(character) = chars.next() {
      match character {
        '\n' => line += 1,
        '/' if !in_comment && chars.peek() == Some(&'*') => in_comment = true,
        '*' if in_comment && chars.peek() == Some(&'/') => in_comment = false,
        '{' if !in_comment => depth += 1,
        '}' if !in_comment => depth -= 1,
        _ => {},
      }
      // More closing than opening braces so far: a stray `}`.
      assert!(depth >= 0, "unexpected `}}` at line {line}");
    }
    assert_eq!(depth, 0, "unclosed block in style/main.css");
  }

  /// The shell is a grid of three rows: banners, top bar, body. A banner placed directly in the grid
  /// takes the top bar's row and pushes the top bar over the channel header.
  #[test]
  fn shell_grid_has_a_row_for_banners_top_bar_and_body() {
    let css = include_str!("../style/main.css");
    let app_rule = css
      .split("\n.app {")
      .nth(1)
      .and_then(|rest| rest.split('}').next())
      .expect("style/main.css has an .app rule");
    let rows = app_rule
      .lines()
      .find_map(|line| line.trim().strip_prefix("grid-template-rows:"))
      .expect(".app sets grid-template-rows");
    assert_eq!(
      rows.trim().trim_end_matches(';'),
      "auto var(--topbar-h) minmax(0, 1fr)"
    );

    let shell = include_str!("components/app.rs");
    // The opening tag spans several lines (its data attributes); the children follow its `>`.
    let grid = shell
      .split("class=\"app\"")
      .nth(1)
      .and_then(|rest| rest.split_once("\n    >\n"))
      .and_then(|(_, rest)| rest.split("      <div\n        class=\"app-body\"").next())
      .expect("the shell has the .app grid with its body");
    // Direct children of the grid sit one indentation level below `<div class="app">`.
    let children: Vec<&str> = grid
      .lines()
      .filter(|line| line.starts_with("      <") && !line.starts_with("      </"))
      .map(str::trim)
      .collect();
    assert_eq!(
      children,
      ["<div class=\"app-banners\" node_ref=banners>", "<TopBar />"]
    );
    for banner in [
      "<BundleBanner />",
      "<OfflineBanner />",
      "<AnnouncementBanner />",
      "<UpdateNotice />",
    ] {
      assert!(
        grid.contains(&format!("        {banner}")),
        "{banner} is inside .app-banners"
      );
    }
  }
}
