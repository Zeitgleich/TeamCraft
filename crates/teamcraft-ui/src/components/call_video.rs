//! Building blocks shared by the pre-join screen and the call screen: self preview, participant tiles
//! and the background effect picker. In a call, tiles show the real video ([`LiveVideo`]): in the
//! browser from the page's call module, on the desktop from the shell; elsewhere (the demo, the
//! pre-join screen) a stylised camera image, so the layouts and effects can be reviewed without
//! media.

use std::time::Duration;

use leptos::prelude::*;
use teamcraft_proto::MediaStreamId;

use crate::calls::BackgroundEffect;
use crate::components::icons::{Icon, IconName};
use crate::components::video_quality::VideoQualityButton;
use crate::components::video_stats::VideoStatsOverlay;
use crate::i18n::use_i18n;
use crate::model::User;
use crate::reactive::{use_interval_while, use_page_visible};
use crate::video_stats::StatsTarget;

/// How long a shown video of another device may go without a new picture before its tile says so
/// (#921): the publisher's uplink or this device's downlink paused it, or it froze on the way.
/// Above the gaps of a video that runs: a still screen sends its picture again every second
/// (`SCREEN_IDLE_REFRESH`), a browser at its frame rate; a camera turned off has no video at all.
const PICTURES_STALLED_AFTER: Duration = Duration::from_secs(3);
/// How often a shown video's count of pictures is looked at.
const PICTURE_CHECK_INTERVAL_MS: u64 = 1_000;

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
  let id = user.id;
  // Their profile picture, like everywhere else (`Avatar`); the initials until it loaded or without.
  let picture = move || crate::profiles::use_profiles().and_then(|hub| hub.avatar_url(id));

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
            move || {
              let initials = initials.clone();
              view! {
                <span class="tile-initials" aria-hidden="true">
                  {move || match picture() {
                    Some(url) => view! { <img class="avatar-img" src=url alt="" /> }.into_any(),
                    None => initials.clone().into_any(),
                  }}
                </span>
              }
            }
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

/// Whether this page can share the screen: the desktop app always, a browser only with
/// `getDisplayMedia` (phones' browsers have none, so the button is hidden there).
pub fn can_share_screen() -> bool {
  #[cfg(target_arch = "wasm32")]
  {
    crate::auth::tauri::is_available() || has_display_media()
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    true
  }
}

#[cfg(target_arch = "wasm32")]
fn has_display_media() -> bool {
  use js_sys::Reflect;
  let Some(window) = web_sys::window() else {
    return false;
  };
  // Looked up by name: `mediaDevices` is missing outside secure contexts, `getDisplayMedia` on phones.
  Reflect::get(&window.navigator(), &"mediaDevices".into())
    .and_then(|devices| Reflect::get(&devices, &"getDisplayMedia".into()))
    .is_ok_and(|function| function.is_function())
}

/// Whether the user can choose the speakers: the desktop app always (its calls run natively), a
/// browser only with `AudioContext.setSinkId`, which routes the call's and the speaker test's
/// sound. Safari has none, so a choice there would do nothing (#841).
pub fn can_choose_speakers() -> bool {
  #[cfg(target_arch = "wasm32")]
  {
    crate::auth::tauri::is_available() || has_sink_id()
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    true
  }
}

#[cfg(target_arch = "wasm32")]
fn has_sink_id() -> bool {
  use js_sys::Reflect;
  // Looked up by name on the prototype: the context itself is only made inside a click.
  Reflect::get(&js_sys::global(), &"AudioContext".into())
    .and_then(|context| Reflect::get(&context, &"prototype".into()))
    .and_then(|prototype| Reflect::has(&prototype, &"setSinkId".into()))
    .unwrap_or(false)
}

/// Real call video: another device's track drawn into a canvas, or the own camera; the call
/// module (browser) or the shell's video (desktop) makes the element. A track is watched only
/// while its tile is on screen and the window is visible, so tracks nobody looks at aren't
/// received or decoded (robustness.md, "Performance and resources").
///
/// Another device's video that gets no new picture for [`PICTURES_STALLED_AFTER`] says so with a
/// short hint (#921), and on the desktop asks for a keyframe once more.
#[component]
pub fn LiveVideo(source: VideoSource, #[prop(into)] is_mirrored: Signal<bool>) -> impl IntoView {
  let i18n = use_i18n();
  let host = NodeRef::<leptos::html::Div>::new();
  let is_page_visible = use_page_visible();
  let is_on_screen = RwSignal::new(true);
  let is_shown = RwSignal::new(false);
  let is_stalled = RwSignal::new(false);
  let picture_watch = StoredValue::new(None::<PictureWatch>);
  let observer = StoredValue::new_local(None::<OnScreenObserver>);
  Effect::new(move |_| {
    let Some(element) = host.get() else {
      return;
    };
    if observer.with_value(Option::is_none) {
      observer.set_value(OnScreenObserver::new(&element, is_on_screen));
    }
    let should_show = is_page_visible.get() && is_on_screen.get();
    if should_show != is_shown.get_untracked() {
      is_shown.set(should_show);
      // Watched afresh from now on: a hidden video gets no pictures by design.
      picture_watch.set_value(None);
      is_stalled.set(false);
      if should_show {
        show(&element, source);
      } else {
        hide(&element, source);
      }
    }
  });
  if let VideoSource::Remote(_) = source {
    use_interval_while(
      PICTURE_CHECK_INTERVAL_MS,
      move || is_shown.get(),
      move || {
        let Some(element) = host.get_untracked() else {
          return;
        };
        let Some(drawn) = pictures_drawn(&element) else {
          return;
        };
        let now = crate::motion::now();
        let flow = picture_watch
          .try_update_value(|maybe_watch| {
            maybe_watch
              .get_or_insert_with(|| PictureWatch::new(now))
              .observe(drawn, now)
          })
          .unwrap_or(PictureFlow::Flowing);
        let has_stalled = flow == PictureFlow::Stalled;
        if has_stalled != is_stalled.get_untracked() {
          is_stalled.set(has_stalled);
          if has_stalled {
            ask_for_keyframe(source);
          }
        }
      },
    );
  }
  on_cleanup(move || {
    if let Some(Some(observer)) = observer.try_update_value(Option::take) {
      observer.disconnect();
    }
    if is_shown.try_get_untracked() == Some(true) {
      stop_watching(source);
    }
  });
  let stats_target = match source {
    VideoSource::Remote(stream_id) => StatsTarget::Received(stream_id),
    VideoSource::OwnCamera => StatsTarget::OwnCamera,
  };
  // Another device's video has a quality menu (#967).
  let maybe_quality = match source {
    VideoSource::Remote(stream_id) => Some(view! { <VideoQualityButton stream_id=stream_id /> }),
    VideoSource::OwnCamera => None,
  };
  view! {
    <div class="live-video" class:mirrored=move || is_mirrored.get() node_ref=host aria-hidden="true"></div>
    <Show when=move || is_stalled.get() fallback=|| ()>
      <span class="video-stalled" role="status">{move || i18n.t("video-stalled")}</span>
    </Show>
    {maybe_quality}
    <VideoStatsOverlay target=stats_target maybe_host=host />
  }
}

/// Whether a shown video still gets pictures, from its count of pictures drawn, looked at now and
/// then: stalled once the count stood still for [`PICTURES_STALLED_AFTER`] since the watch began
/// or since it last moved. Time is passed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PictureWatch {
  maybe_last_count: Option<u64>,
  /// When the count last moved, or the watch began.
  since: Duration,
}

/// What a [`PictureWatch`] makes of a video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PictureFlow {
  Flowing,
  Stalled,
}

impl PictureWatch {
  fn new(now: Duration) -> Self {
    Self {
      maybe_last_count: None,
      since: now,
    }
  }

  /// Takes the count of pictures drawn so far, read at `now`.
  fn observe(&mut self, count: u64, now: Duration) -> PictureFlow {
    if self.maybe_last_count.is_some_and(|last| last != count) {
      self.since = now;
    }
    self.maybe_last_count = Some(count);
    if now.saturating_sub(self.since) >= PICTURES_STALLED_AFTER {
      PictureFlow::Stalled
    } else {
      PictureFlow::Flowing
    }
  }
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

/// Puts the element for `source` into `host`.
#[cfg(target_arch = "wasm32")]
fn show(host: &web_sys::HtmlDivElement, source: VideoSource) {
  if let Some(element) = shown::show(source) {
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
  shown::hide(source);
}

/// The pictures drawn so far into the element `host` shows; `None` where nothing counts them.
#[cfg(target_arch = "wasm32")]
fn pictures_drawn(host: &web_sys::HtmlDivElement) -> Option<u64> {
  let element = host.first_element_child()?;
  teamcraft_media::web_api::pictures_drawn(&element)
}

/// A shown track stopped getting pictures: the desktop shell asks for a keyframe once more. (The
/// browser's call module asks by itself whenever it can't decode.)
#[cfg(target_arch = "wasm32")]
fn ask_for_keyframe(source: VideoSource) {
  if crate::shell_video::can_show_video() {
    crate::shell_video::request_keyframe(source);
  }
}

/// The video elements tiles show, one per source, from the shell on the desktop and from the call
/// module in the browser. A source no tile shows any more keeps running for [`shown::LINGER`], so
/// a re-layout (the speaker changed, a tile scrolled back in, the first visibility report) reuses
/// the running stream instead of restarting it and waiting for a keyframe. Two tiles showing the
/// same source at once share the element: it moves to the newer one.
#[cfg(target_arch = "wasm32")]
mod shown {
  use std::cell::RefCell;
  use std::collections::HashMap;
  use std::time::Duration;

  use leptos::task::spawn_local;
  use web_sys::Element;

  use super::VideoSource;

  pub const LINGER: Duration = Duration::from_millis(500);

  thread_local! {
    static SHOWN: RefCell<HashMap<VideoSource, Shown>> = RefCell::default();
  }

  struct Shown {
    element: Element,
    tiles: u32,
    /// Changes with every show and hide, so a linger timer of an earlier hide sees that the
    /// source was shown again meanwhile.
    generation: u64,
  }

  pub fn show(source: VideoSource) -> Option<Element> {
    let maybe_element = SHOWN.with(|shown| {
      shown.borrow_mut().get_mut(&source).map(|entry| {
        entry.tiles += 1;
        entry.generation += 1;
        entry.element.clone()
      })
    });
    if maybe_element.is_some() {
      return maybe_element;
    }
    let element = start(source)?;
    SHOWN.with(|shown| {
      shown.borrow_mut().insert(
        source,
        Shown {
          element: element.clone(),
          tiles: 1,
          generation: 0,
        },
      )
    });
    Some(element)
  }

  pub fn hide(source: VideoSource) {
    let maybe_generation = SHOWN.with(|shown| {
      let mut shown = shown.borrow_mut();
      let entry = shown.get_mut(&source)?;
      entry.tiles = entry.tiles.saturating_sub(1);
      entry.generation += 1;
      (entry.tiles == 0).then_some(entry.generation)
    });
    let Some(generation) = maybe_generation else {
      return;
    };
    spawn_local(async move {
      teamcraft_core::platform::sleep(LINGER).await;
      stop_if_unshown(source, generation);
    });
  }

  /// Stops `source` unless it was shown again since the hide of `generation`.
  fn stop_if_unshown(source: VideoSource, generation: u64) {
    let is_unshown = SHOWN.with(|shown| {
      let mut shown = shown.borrow_mut();
      let is_unshown = shown
        .get(&source)
        .is_some_and(|entry| entry.tiles == 0 && entry.generation == generation);
      if is_unshown {
        shown.remove(&source);
      }
      is_unshown
    });
    if is_unshown {
      stop(source);
    }
  }

  fn start(source: VideoSource) -> Option<Element> {
    if crate::shell_video::can_show_video() {
      return crate::shell_video::show(source);
    }
    match source {
      VideoSource::Remote(stream_id) => crate::web_calls::watch_video(stream_id),
      VideoSource::OwnCamera => crate::web_calls::camera_view(),
    }
  }

  fn stop(source: VideoSource) {
    if crate::shell_video::can_show_video() {
      crate::shell_video::hide(source);
    } else if let VideoSource::Remote(stream_id) = source {
      crate::web_calls::unwatch_video(stream_id);
    }
  }
}

#[cfg(not(target_arch = "wasm32"))]
fn show(_host: &web_sys::HtmlDivElement, _source: VideoSource) {}

#[cfg(not(target_arch = "wasm32"))]
fn hide(_host: &web_sys::HtmlDivElement, _source: VideoSource) {}

#[cfg(not(target_arch = "wasm32"))]
fn stop_watching(_source: VideoSource) {}

#[cfg(not(target_arch = "wasm32"))]
fn pictures_drawn(_host: &web_sys::HtmlDivElement) -> Option<u64> {
  None
}

#[cfg(not(target_arch = "wasm32"))]
fn ask_for_keyframe(_source: VideoSource) {}

pub use crate::components::background_picker::{BackgroundPicker, PickerSize};

#[cfg(test)]
mod tests {
  use super::*;

  const START: Duration = Duration::from_secs(100);

  #[test]
  fn a_video_whose_pictures_keep_coming_flows() {
    let mut watch = PictureWatch::new(START);
    for second in 0..10u64 {
      let flow = watch.observe(second * 30, START + Duration::from_secs(second));
      assert_eq!(flow, PictureFlow::Flowing);
    }
  }

  #[test]
  fn a_still_screen_refreshed_every_second_flows() {
    let mut watch = PictureWatch::new(START);
    // Checked every second; the picture is sent again about as often, sometimes a bit later.
    let counts = [0, 1, 1, 2, 3, 3, 4, 5, 5, 6];
    for (second, count) in (0..).zip(counts) {
      let flow = watch.observe(count, START + Duration::from_secs(second));
      assert_eq!(flow, PictureFlow::Flowing, "at {second} s");
    }
  }

  #[test]
  fn a_video_without_new_pictures_stalls_and_recovers() {
    let mut watch = PictureWatch::new(START);
    assert_eq!(watch.observe(40, START), PictureFlow::Flowing);
    assert_eq!(
      watch.observe(40, START + Duration::from_secs(2)),
      PictureFlow::Flowing
    );
    assert_eq!(
      watch.observe(40, START + PICTURES_STALLED_AFTER),
      PictureFlow::Stalled
    );
    // A picture again: flowing at once.
    assert_eq!(
      watch.observe(41, START + Duration::from_secs(4)),
      PictureFlow::Flowing
    );
  }

  #[test]
  fn a_video_that_never_got_a_picture_stalls() {
    // A late viewer waiting on a layer nobody sends: black, and now it says so.
    let mut watch = PictureWatch::new(START);
    assert_eq!(
      watch.observe(0, START + Duration::from_secs(1)),
      PictureFlow::Flowing
    );
    assert_eq!(
      watch.observe(0, START + PICTURES_STALLED_AFTER),
      PictureFlow::Stalled
    );
  }

  #[test]
  fn a_reused_element_counts_from_the_first_look() {
    // A canvas kept for a moment after its tile went (`shown::LINGER`) carries its old count.
    let mut watch = PictureWatch::new(START);
    assert_eq!(watch.observe(500, START), PictureFlow::Flowing);
    assert_eq!(
      watch.observe(501, START + Duration::from_secs(2)),
      PictureFlow::Flowing
    );
    assert_eq!(
      watch.observe(501, START + Duration::from_secs(4)),
      PictureFlow::Flowing
    );
  }
}
