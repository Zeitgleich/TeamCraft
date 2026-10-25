//! The viewport: which layout the window's width calls for, the on-screen keyboard, the input
//! method and the browser's theme color. Phones get one pane at a time, tablets add the sidebar,
//! wider windows the full desktop layout (issue #307).

use std::cell::Cell;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

/// Below this width (CSS pixels) one pane shows at a time, with the bottom navigation.
pub const COMPACT_BELOW_PX: u32 = 768;
/// Below this height a window that isn't wide is compact too: a phone in landscape has no room
/// for the sidebar beside the conversation once the keyboard is open.
pub const COMPACT_BELOW_HEIGHT_PX: u32 = 500;
/// Below this width the sidebar shows but side panels lie over the conversation.
pub const MEDIUM_BELOW_PX: u32 = 1100;

/// The layouts the shell switches between, mirrored into `data-layout` on `<html>` for the CSS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
  /// Phones: list, conversation and thread each fill the window; a bottom navigation.
  Compact,
  /// Tablets and narrow windows: rail and sidebar beside the conversation.
  Medium,
  /// The desktop layout with side panels beside the conversation.
  #[default]
  Wide,
}

/// Which pane fills the window in the compact layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactPane {
  /// The list of the selected bottom-navigation tab: Home's DMs or a space's channels.
  List,
  /// The main column: a conversation, Activity, search, …
  Main,
  /// The side panel over the conversation: a thread, pins or members.
  Panel,
}

impl Layout {
  /// The layout for a viewport of this size.
  pub fn for_size(width_px: f64, height_px: f64) -> Self {
    let is_wide = width_px >= f64::from(MEDIUM_BELOW_PX);
    if width_px < f64::from(COMPACT_BELOW_PX) || (height_px < f64::from(COMPACT_BELOW_HEIGHT_PX) && !is_wide)
    {
      Self::Compact
    } else if !is_wide {
      Self::Medium
    } else {
      Self::Wide
    }
  }

  /// The desktop app never takes the phone layout: a narrow window keeps the rail and sidebar
  /// (with panels over the conversation), as desktop chat apps do.
  pub fn for_desktop_app(self) -> Self {
    match self {
      Self::Compact => Self::Medium,
      other => other,
    }
  }

  pub fn is_compact(self) -> bool {
    self == Self::Compact
  }

  pub fn slug(self) -> &'static str {
    match self {
      Self::Compact => "compact",
      Self::Medium => "medium",
      Self::Wide => "wide",
    }
  }
}

impl CompactPane {
  pub fn slug(self) -> &'static str {
    match self {
      Self::List => "list",
      Self::Main => "main",
      Self::Panel => "panel",
    }
  }
}

/// The layout of the window right now.
pub fn current_layout() -> Layout {
  // Native unit tests have no window (and `web_sys` panics there).
  if !cfg!(target_arch = "wasm32") {
    return Layout::Wide;
  }
  let Some(window) = web_sys::window() else {
    return Layout::Wide;
  };
  let size =
    |value: Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>| value.ok().and_then(|value| value.as_f64());
  let layout = match (size(window.inner_width()), size(window.inner_height())) {
    (Some(width), Some(height)) => Layout::for_size(width, height),
    _ => Layout::Wide,
  };
  if crate::auth::tauri::is_available() {
    layout.for_desktop_app()
  } else {
    layout
  }
}

thread_local! {
  /// The layout signal of the shell on screen; the media query listeners (registered once per
  /// page) update it. A new sign-in mounts a new shell with a new signal.
  static WATCHED_LAYOUT: Cell<Option<RwSignal<Layout>>> = const { Cell::new(None) };
}

/// Keeps `layout` equal to the window's layout and mirrors it into `data-layout` on `<html>`.
/// Media query listeners fire only when a breakpoint is crossed, not on every resize.
pub fn watch_layout(layout: RwSignal<Layout>) {
  Effect::new(move |_| {
    let slug = layout.get().slug();
    if let Some(root) = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|document| document.document_element())
      && root.set_attribute("data-layout", slug).is_err()
    {
      tracing::debug!("could not apply the layout attribute");
    }
  });
  if WATCHED_LAYOUT.replace(Some(layout)).is_some() {
    return;
  }
  let Some(window) = web_sys::window() else { return };
  let queries = [
    format!("(max-width: {}px)", f64::from(COMPACT_BELOW_PX) - 0.02),
    format!("(max-width: {}px)", f64::from(MEDIUM_BELOW_PX) - 0.02),
    format!("(max-height: {}px)", f64::from(COMPACT_BELOW_HEIGHT_PX) - 0.02),
  ];
  for query in queries {
    let Ok(Some(list)) = window.match_media(&query) else {
      continue;
    };
    let handler = Closure::<dyn Fn()>::new(move || {
      // async-ok: a thread-local `Cell`; the signal inside is only touched with try_ below.
      let Some(layout) = WATCHED_LAYOUT.get() else {
        return;
      };
      let next = current_layout();
      // Between sign-out and the next shell the signal is gone; then nothing happens.
      if layout.try_get_untracked().is_some_and(|current| current != next) {
        let _ = layout.try_set(next);
      }
    });
    if list
      .add_event_listener_with_callback("change", handler.as_ref().unchecked_ref())
      .is_ok()
    {
      // The listener lives as long as the page.
      handler.forget();
    }
  }
}

/// Whether the primary input is a finger (phones, tablets): Enter inserts a newline there and the
/// send button sends, as in the platforms' own messengers.
pub fn is_touch_primary() -> bool {
  web_sys::window()
    .and_then(|window| window.match_media("(pointer: coarse)").ok().flatten())
    .is_some_and(|query| query.matches())
}

/// Size of one CSS `rem` in pixels: the browser's default text size times the text size setting
/// (issue #370). Sizes that follow the text, such as menus measured before they render, scale
/// with it. 16 when the root can't be read.
pub fn rem_px() -> f64 {
  web_sys::window()
    .and_then(|window| {
      let root = window.document()?.document_element()?;
      window.get_computed_style(&root).ok().flatten()
    })
    .and_then(|style| style.get_property_value("font-size").ok())
    .and_then(|size| size.trim_end_matches("px").parse::<f64>().ok())
    .filter(|px| *px > 0.0)
    .unwrap_or(16.0)
}

/// Mirrors the visible part of the window into `--vv-height` and `--vv-top` on `<html>`, so the
/// compact layout ends at the on-screen keyboard and the composer stays above it. Chrome resizes
/// the layout itself (`interactive-widget=resizes-content` in index.html); Safari only reports the
/// visual viewport.
pub fn track_visual_viewport() {
  thread_local! {
    static IS_TRACKING: Cell<bool> = const { Cell::new(false) };
  }
  // Once per page: the shell mounts again after a new sign-in.
  if IS_TRACKING.replace(true) {
    return;
  }
  let Some(viewport) = web_sys::window().and_then(|window| window.visual_viewport()) else {
    return;
  };
  let apply = {
    let viewport = viewport.clone();
    move || {
      let Some(root) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.document_element())
        .and_then(|root| root.dyn_into::<web_sys::HtmlElement>().ok())
      else {
        return;
      };
      let style = root.style();
      let height = format!("{}px", viewport.height().round());
      let top = format!("{}px", viewport.offset_top().max(0.0).round());
      if style.set_property("--vv-height", &height).is_err() || style.set_property("--vv-top", &top).is_err()
      {
        tracing::debug!("could not apply the visual viewport size");
      }
    }
  };
  apply();
  let handler = Closure::<dyn Fn()>::new(apply);
  for event in ["resize", "scroll"] {
    if viewport
      .add_event_listener_with_callback(event, handler.as_ref().unchecked_ref())
      .is_err()
    {
      tracing::debug!(event, "could not watch the visual viewport");
    }
  }
  // The listeners live as long as the page.
  handler.forget();
}

/// Sets `<meta name="theme-color">` (the phone's status bar, an installed app's title bar) to the
/// top bar's color of the current theme and brand, whenever the theme changes.
pub fn sync_theme_color(theme_changed: impl Fn() + 'static) {
  Effect::new(move |_| {
    theme_changed();
    // The brand's stylesheet and `data-theme` apply before the next frame.
    request_animation_frame(apply_theme_color);
  });
}

fn apply_theme_color() {
  let Some(window) = web_sys::window() else { return };
  let Some(document) = window.document() else { return };
  let Some(root) = document.document_element() else {
    return;
  };
  let Ok(Some(style)) = window.get_computed_style(&root) else {
    return;
  };
  let Ok(color) = style.get_property_value("--chrome") else {
    return;
  };
  let color = color.trim();
  if color.is_empty() {
    return;
  }
  let maybe_meta = document
    .query_selector("meta[name=\"theme-color\"]")
    .ok()
    .flatten()
    .or_else(|| {
      let meta = document.create_element("meta").ok()?;
      meta.set_attribute("name", "theme-color").ok()?;
      document.head()?.append_child(&meta).ok()?;
      Some(meta)
    });
  if let Some(meta) = maybe_meta
    && meta.set_attribute("content", color).is_err()
  {
    tracing::debug!("could not set the theme color");
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn phones_get_the_compact_layout_in_portrait_and_landscape() {
    for (width, height) in [
      (360.0, 740.0),
      (390.0, 844.0),
      (412.0, 915.0),
      (767.9, 1024.0),
      (844.0, 390.0),
      (915.0, 412.0),
    ] {
      assert_eq!(
        Layout::for_size(width, height),
        Layout::Compact,
        "{width}x{height}"
      );
    }
  }

  #[test]
  fn tablets_get_the_sidebar_and_wide_windows_the_desktop_layout() {
    assert_eq!(Layout::for_size(768.0, 1024.0), Layout::Medium);
    assert_eq!(Layout::for_size(1024.0, 768.0), Layout::Medium);
    assert_eq!(Layout::for_size(1099.0, 800.0), Layout::Medium);
    assert_eq!(Layout::for_size(1100.0, 800.0), Layout::Wide);
    assert_eq!(Layout::for_size(1920.0, 1080.0), Layout::Wide);
  }

  #[test]
  fn the_desktop_app_never_gets_the_phone_layout() {
    assert_eq!(Layout::for_size(720.0, 800.0).for_desktop_app(), Layout::Medium);
    assert_eq!(Layout::for_size(1000.0, 480.0).for_desktop_app(), Layout::Medium);
    assert_eq!(Layout::for_size(1400.0, 900.0).for_desktop_app(), Layout::Wide);
  }

  #[test]
  fn a_short_window_is_compact_only_while_it_is_not_wide() {
    assert_eq!(Layout::for_size(1000.0, 480.0), Layout::Compact);
    assert_eq!(Layout::for_size(1400.0, 480.0), Layout::Wide);
  }
}
