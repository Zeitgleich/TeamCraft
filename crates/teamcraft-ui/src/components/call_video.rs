//! Building blocks shared by the pre-join screen and the call screen: self preview, participant tiles
//! and the background effect picker. Where the media runs in the page (the browser), tiles show the
//! real video ([`LiveVideo`]); elsewhere a stylised camera image, so the layouts and effects can be
//! reviewed without media.

use leptos::prelude::*;
use teamcraft_proto::MediaStreamId;

use crate::calls::BackgroundEffect;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::User;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoSource {
  /// Another device's track in the call.
  Remote(MediaStreamId),
  /// The camera this device sends.
  OwnCamera,
}

/// Whether this page shows real call video (the browser, once the call module runs).
pub fn can_show_video() -> bool {
  #[cfg(target_arch = "wasm32")]
  {
    crate::web_calls::can_show_video()
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    false
  }
}

/// Real call video: another device's track drawn into a canvas, or the own camera in a muted
/// `<video>`; the call module makes the element. Watching starts when it is shown and stops when it
/// goes away, so tracks nobody looks at aren't received (robustness.md, "Performance and resources").
#[component]
pub fn LiveVideo(source: VideoSource, #[prop(into)] is_mirrored: Signal<bool>) -> impl IntoView {
  let host = NodeRef::<leptos::html::Div>::new();
  let state = crate::state::use_app_state();
  Effect::new(move |_| {
    // Again whenever the call changes: a camera switched to another device is a new stream.
    if source == VideoSource::OwnCamera {
      state.read(|backend| backend.active_call().map(|active| active.is_camera_on));
    }
    if let Some(element) = host.get() {
      show(&element, source);
    }
  });
  on_cleanup(move || {
    if let VideoSource::Remote(stream_id) = source {
      unwatch(stream_id);
    }
  });
  view! { <div class="live-video" class:mirrored=move || is_mirrored.get() node_ref=host aria-hidden="true"></div> }
}

/// Puts the call module's element for `source` into `host`, replacing an older one.
#[cfg(target_arch = "wasm32")]
fn show(host: &web_sys::HtmlDivElement, source: VideoSource) {
  let maybe_element = match source {
    // Watched once; the canvas keeps showing the track.
    VideoSource::Remote(_) if host.child_element_count() > 0 => return,
    VideoSource::Remote(stream_id) => crate::web_calls::watch_video(stream_id),
    VideoSource::OwnCamera => crate::web_calls::camera_view(),
  };
  if let Some(element) = maybe_element {
    host.replace_children_with_node_1(&element);
  }
}

#[cfg(target_arch = "wasm32")]
fn unwatch(stream_id: MediaStreamId) {
  crate::web_calls::unwatch_video(stream_id);
}

#[cfg(not(target_arch = "wasm32"))]
fn show(_host: &web_sys::HtmlDivElement, _source: VideoSource) {}

#[cfg(not(target_arch = "wasm32"))]
fn unwatch(_stream_id: MediaStreamId) {}

pub use crate::components::background_picker::BackgroundPicker;
