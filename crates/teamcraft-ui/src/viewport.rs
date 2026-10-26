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
/// A visual viewport at least this much shorter than the layout has the keyboard below it; less is
/// browser chrome or the home indicator (#663).
const KEYBOARD_MIN_HEIGHT_PX: f64 = 120.0;

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

/// How deep a page of the compact layout is: the pages of the bottom navigation, a page opened
/// from them (a conversation, search), a side panel over it (a thread).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompactDepth {
  TopLevel,
  Page,
  Panel,
}

/// Which way the compact layout just moved (#368): deeper pages slide in from the trailing edge,
/// going back slides the parent in from the leading edge. Between pages of the same depth (the
/// bottom navigation's tabs) nothing slides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaneStep {
  #[default]
  Lateral,
  Push,
  Pop,
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

impl PaneStep {
  pub fn between(before: CompactDepth, after: CompactDepth) -> Self {
    match after.cmp(&before) {
      std::cmp::Ordering::Greater => Self::Push,
      std::cmp::Ordering::Less => Self::Pop,
      std::cmp::Ordering::Equal => Self::Lateral,
    }
  }

  /// The value of `data-nav` on `.app`; `None` leaves the attribute off.
  pub fn slug(self) -> Option<&'static str> {
    match self {
      Self::Lateral => None,
      Self::Push => Some("push"),
      Self::Pop => Some("pop"),
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

/// Mirrors the visible part of the window into `--vv-height` and `--vv-top` on `<html>` while the
/// on-screen keyboard covers part of it, so the compact layout ends at the keyboard and the composer
/// stays above it. Chrome resizes the layout itself (`interactive-widget=resizes-content` in
/// index.html); Safari only reports the visual viewport.
///
/// Without the keyboard the properties are removed and the shell fills `100dvh` (#663): iOS reports
/// a visual viewport shorter than the screen in an installed web app and under Safari's floating
/// toolbar, and keeps a shrunken one after the keyboard closed. The shell then ended above the
/// screen's bottom, and the page showed through below the tab bar.
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
      let layout_height = web_sys::window()
        .and_then(|window| window.inner_height().ok())
        .and_then(|height| height.as_f64())
        .unwrap_or_else(|| viewport.height());
      let maybe_area = keyboard_free_area(
        layout_height,
        viewport.height(),
        viewport.offset_top(),
        viewport.scale(),
        is_text_field_focused(),
      );
      let is_applied = match maybe_area {
        Some((height, top)) => {
          style
            .set_property("--vv-height", &format!("{}px", height.round()))
            .is_ok()
            && style
              .set_property("--vv-top", &format!("{}px", top.round()))
              .is_ok()
        },
        None => style.remove_property("--vv-height").is_ok() && style.remove_property("--vv-top").is_ok(),
      };
      if !is_applied {
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
  // A keyboard that closes without a resize (iOS) still gives the full height back. Focus moving
  // between two fields passes `focusout` then `focusin` in one task, before anything is drawn.
  if let Some(document) = web_sys::window().and_then(|window| window.document()) {
    for event in ["focusin", "focusout"] {
      if document
        .add_event_listener_with_callback(event, handler.as_ref().unchecked_ref())
        .is_err()
      {
        tracing::debug!(event, "could not watch the focus for the visual viewport");
      }
    }
  }
  // The listeners live as long as the page.
  handler.forget();
}

/// The part of the window the compact layout fits into while the keyboard is up, as height and
/// offset from the top in CSS pixels, or `None` when the shell should fill `100dvh`: no text field
/// has the focus (no keyboard), the page is pinch-zoomed (the visual viewport is a zoomed window
/// then, not the free area), or the visual viewport is about as tall as the layout.
fn keyboard_free_area(
  layout_height: f64,
  visible_height: f64,
  visible_top: f64,
  scale: f64,
  is_text_field_focused: bool,
) -> Option<(f64, f64)> {
  let is_zoomed = scale > 1.01;
  let is_covered = layout_height - visible_height > KEYBOARD_MIN_HEIGHT_PX;
  (is_text_field_focused && !is_zoomed && is_covered).then_some((visible_height, visible_top.max(0.0)))
}

/// Whether the focus is in something the on-screen keyboard types into.
fn is_text_field_focused() -> bool {
  web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element())
    .is_some_and(|element| {
      element
        .matches("input, textarea, [contenteditable]:not([contenteditable=false])")
        .unwrap_or(false)
    })
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

  mod keyboard_free_area {
    use super::*;

    #[test]
    fn follows_the_visual_viewport_while_the_keyboard_is_up() {
      assert_eq!(
        keyboard_free_area(844.0, 508.0, 0.0, 1.0, true),
        Some((508.0, 0.0))
      );
      assert_eq!(
        keyboard_free_area(844.0, 508.0, -3.0, 1.0, true),
        Some((508.0, 0.0))
      );
    }

    #[test]
    fn fills_the_screen_without_a_focused_text_field() {
      // iOS keeps a shrunken visual viewport after the keyboard closed.
      assert_eq!(keyboard_free_area(844.0, 508.0, 0.0, 1.0, false), None);
    }

    #[test]
    fn ignores_browser_chrome_and_the_home_indicator() {
      assert_eq!(keyboard_free_area(844.0, 778.0, 0.0, 1.0, true), None);
      assert_eq!(keyboard_free_area(844.0, 844.0, 0.0, 1.0, true), None);
    }

    #[test]
    fn ignores_a_pinch_zoomed_page() {
      assert_eq!(keyboard_free_area(844.0, 422.0, 200.0, 2.0, true), None);
    }
  }

  #[test]
  fn deeper_is_a_push_shallower_a_pop_and_same_depth_lateral() {
    use CompactDepth::{Page, Panel, TopLevel};
    assert_eq!(PaneStep::between(TopLevel, Page), PaneStep::Push);
    assert_eq!(PaneStep::between(Page, Panel), PaneStep::Push);
    assert_eq!(PaneStep::between(Panel, TopLevel), PaneStep::Pop);
    assert_eq!(PaneStep::between(Page, TopLevel), PaneStep::Pop);
    assert_eq!(PaneStep::between(TopLevel, TopLevel), PaneStep::Lateral);
    assert_eq!(PaneStep::Lateral.slug(), None);
  }

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
