//! Building blocks shared by the pre-join screen and the call screen: self preview, participant tiles
//! and the background effect picker. In a call, tiles show the real video ([`LiveVideo`]): in the
//! browser from the page's call module, on the desktop from the shell; elsewhere (the demo, the
//! pre-join screen) a stylised camera image, so the layouts and effects can be reviewed without
//! media.

use leptos::prelude::*;
use teamcraft_proto::MediaStreamId;

use crate::calls::BackgroundEffect;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::User;
use crate::reactive::use_page_visible;

/// A participant tile (or the self preview). `effect` only applies to the user's own camera.
#[component]
pub fn VideoTile(
  user: User,
  #[prop(default = false)] is_self: bool,
  #[prop(into)] is_muted: Signal<bool>,
  #[prop(into)] is_camera_on: Signal<bool>,
  #[prop(into, default = Signal::stored(false))] is_speaking: Signal<bool>,
  #[prop(into, default = Signal::stored(false))] has_hand_raised: Signal<bool>,
  /// The person's connection was lost and they are rejoining.
  #[prop(into, default = Signal::stored(false))]
  is_reconnecting: Signal<bool>,
  #[prop(into, default = Signal::stored(BackgroundEffect::None))] effect: Signal<BackgroundEffect>,
  #[prop(into, default = Signal::stored(false))] is_mirrored: Signal<bool>,
  #[prop(default = false)] is_large: bool,
  /// The real video of the tile, where the page shows call video.
  #[prop(into, default = Signal::stored(None))]
  video: Signal<Option<VideoSource>>,
) -> impl IntoView {
  let i18n = use_i18n();
  let name = if is_self {
    String::new()
  } else {
    user.display_name.clone()
  };
  let display = user.display_name.clone();
  let hue = user.avatar_hue;
  let initials = user.initials();

  view! {
    <div
      class="tile"
      class:tile-speaking=move || is_speaking.get()
      class:tile-reconnecting=move || is_reconnecting.get()
      class:tile-large=is_large
      style:--hue=hue.to_string()
      role="group"
      aria-label=move || {
        let mut parts = vec![if is_self { i18n.t("tile-you") } else { display.clone() }];
        if is_muted.get() {
          parts.push(i18n.t("tile-muted"));
        }
        if has_hand_raised.get() {
          parts.push(i18n.t("tile-hand"));
        }
        if is_speaking.get() {
          parts.push(i18n.t("tile-speaking"));
        }
        if is_reconnecting.get() {
          parts.push(i18n.t("tile-reconnecting"));
        }
        parts.join(", ")
      }
    >
      <div class=move || format!("video-surface effect-{}", if is_self { effect.get().slug() } else { "none" })>
        <Show
          when=move || is_camera_on.get()
          fallback={
            let initials = initials.clone();
            move || view! { <span class="tile-initials" aria-hidden="true">{initials.clone()}</span> }
          }
        >
          {move || match video.get() {
            Some(source) => view! { <LiveVideo source=source is_mirrored=is_mirrored /> }.into_any(),
            None => view! {
              <div class="scene" aria-hidden="true"></div>
              <div class="silhouette" class:mirrored=move || is_mirrored.get() aria-hidden="true"></div>
            }
            .into_any(),
          }}
        </Show>
      </div>
      <div class="tile-label">
        {(!is_self).then(|| view! { <span class="tile-name" translate="no">{name.clone()}</span> })}
        {is_self.then(|| view! { <span class="tile-name">{move || i18n.t("tile-you")}</span> })}
        <Show when=move || is_muted.get() fallback=|| ()>
          <Icon name=IconName::MicOff class="tile-badge" />
        </Show>
      </div>
      <Show when=move || is_reconnecting.get() fallback=|| ()>
        <span class="tile-status" aria-hidden="true">{move || i18n.t("tile-reconnecting")}</span>
      </Show>
      <Show when=move || has_hand_raised.get() fallback=|| ()>
        <span class="tile-hand" aria-hidden="true">
          <Icon name=IconName::Hand />
        </span>
      </Show>
    </div>
  }
}

/// Where a tile's real video comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoSource {
  /// Another device's track in the call.
  Remote(MediaStreamId),
  /// The camera this device sends.
  OwnCamera,
}

/// Whether this page shows real call video: the desktop app (the shell sends it, `shell_video`),
/// or the browser once the call module runs.
pub fn can_show_video() -> bool {
  #[cfg(target_arch = "wasm32")]
  {
    crate::shell_video::can_show_video() || crate::web_calls::can_show_video()
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    false
  }
}

/// Real call video: another device's track drawn into a canvas, or the own camera; the call
/// module (browser) or the shell's video (desktop) makes the element. A track is watched only
/// while its tile is on screen and the window is visible, so tracks nobody looks at aren't
/// received or decoded (robustness.md, "Performance and resources").
#[component]
pub fn LiveVideo(source: VideoSource, #[prop(into)] is_mirrored: Signal<bool>) -> impl IntoView {
  let host = NodeRef::<leptos::html::Div>::new();
  let is_page_visible = use_page_visible();
  let is_on_screen = RwSignal::new(true);
  let is_shown = StoredValue::new(false);
  let observer = StoredValue::new_local(None::<OnScreenObserver>);
  Effect::new(move |_| {
    let Some(element) = host.get() else {
      return;
    };
    if observer.with_value(Option::is_none) {
      observer.set_value(OnScreenObserver::new(&element, is_on_screen));
    }
    let should_show = is_page_visible.get() && is_on_screen.get();
    if should_show != is_shown.get_value() {
      is_shown.set_value(should_show);
      if should_show {
        show(&element, source);
      } else {
        hide(&element, source);
      }
    }
  });
  on_cleanup(move || {
    if let Some(Some(observer)) = observer.try_update_value(Option::take) {
      observer.disconnect();
    }
    if is_shown.try_get_value() == Some(true) {
      stop_watching(source);
    }
  });
  view! { <div class="live-video" class:mirrored=move || is_mirrored.get() node_ref=host aria-hidden="true"></div> }
}

/// Follows whether an element is on screen (not scrolled out of a filmstrip).
#[cfg(target_arch = "wasm32")]
struct OnScreenObserver {
  observer: web_sys::IntersectionObserver,
  _callback: wasm_bindgen::closure::Closure<dyn FnMut(js_sys::Array)>,
}

#[cfg(target_arch = "wasm32")]
impl OnScreenObserver {
  fn new(element: &web_sys::HtmlDivElement, is_on_screen: RwSignal<bool>) -> Option<Self> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    use crate::reactive::SetIfAlive;

    let callback = Closure::<dyn FnMut(js_sys::Array)>::new(move |entries: js_sys::Array| {
      // The last entry is the newest state of the one observed element.
      if let Some(entry) = entries
        .iter()
        .last()
        .and_then(|entry| entry.dyn_into::<web_sys::IntersectionObserverEntry>().ok())
      {
        is_on_screen.set_if_alive(entry.is_intersecting());
      }
    });
    let observer = web_sys::IntersectionObserver::new(callback.as_ref().unchecked_ref()).ok()?;
    observer.observe(element);
    Some(Self {
      observer,
      _callback: callback,
    })
  }

  fn disconnect(&self) {
    self.observer.disconnect();
  }
}

#[cfg(not(target_arch = "wasm32"))]
struct OnScreenObserver;

#[cfg(not(target_arch = "wasm32"))]
impl OnScreenObserver {
  fn new(_element: &web_sys::HtmlDivElement, _is_on_screen: RwSignal<bool>) -> Option<Self> {
    None
  }

  fn disconnect(&self) {}
}

/// Puts the element for `source` into `host`: from the shell on the desktop, from the call module
/// in the browser.
#[cfg(target_arch = "wasm32")]
fn show(host: &web_sys::HtmlDivElement, source: VideoSource) {
  let maybe_element = if crate::shell_video::can_show_video() {
    crate::shell_video::show(source)
  } else {
    match source {
      VideoSource::Remote(stream_id) => crate::web_calls::watch_video(stream_id),
      VideoSource::OwnCamera => crate::web_calls::camera_view(),
    }
  };
  if let Some(element) = maybe_element {
    host.replace_children_with_node_1(&element);
  }
}

/// The tile went off screen or the window was hidden: the video stops until it is shown again.
#[cfg(target_arch = "wasm32")]
fn hide(host: &web_sys::HtmlDivElement, source: VideoSource) {
  stop_watching(source);
  host.replace_children_with_node_0();
}

#[cfg(target_arch = "wasm32")]
fn stop_watching(source: VideoSource) {
  if crate::shell_video::can_show_video() {
    crate::shell_video::hide(source);
  } else if let VideoSource::Remote(stream_id) = source {
    crate::web_calls::unwatch_video(stream_id);
  }
}

#[cfg(not(target_arch = "wasm32"))]
fn show(_host: &web_sys::HtmlDivElement, _source: VideoSource) {}

#[cfg(not(target_arch = "wasm32"))]
fn hide(_host: &web_sys::HtmlDivElement, _source: VideoSource) {}

#[cfg(not(target_arch = "wasm32"))]
fn stop_watching(_source: VideoSource) {}

pub use crate::components::background_picker::BackgroundPicker;
