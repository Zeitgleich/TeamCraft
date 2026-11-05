//! The app's own video player (#638), for videos playing in their message row and in the lightbox:
//! play and pause, a scrubber that shows what is buffered and the time under the pointer, the time,
//! speed, repeat, mute and volume, and full screen (#200), drawn with the app's icons on every platform instead of each webview's native controls
//! (WebKitGTK's GStreamer bar, WKWebView's, WebView2's). Keys work while the player has focus; the
//! logic behind them is in [`crate::video_controls`]. A video that gives no sign of playing counts
//! as unplayable, like one the webview reports an error for (#804).

use std::time::Duration;

use leptos::ev;
use leptos::html;
use leptos::leptos_dom::helpers::{TimeoutHandle, set_timeout_with_handle};
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::reactive::SetIfAlive;
use crate::video_controls::{
  PlaybackState, PlayerCommand, STALL_TIMEOUT, StallCheck, buffered_until, format_seconds, format_speed,
  next_speed, percent_of, scrubber_time, seek_target, tenth_target, volume_target,
};

/// The volume unmuting goes back to when the volume was turned all the way down.
const UNMUTED_VOLUME: f64 = 0.5;
/// How long the pointer rests before the controls of a playing video go.
const CONTROLS_IDLE_TIMEOUT: Duration = Duration::from_millis(2500);
/// `HTMLMediaElement.readyState` values.
const HAVE_METADATA: u16 = 1;
const HAVE_CURRENT_DATA: u16 = 2;

/// A `<video>` without the platform's controls and with the app's own over it. The player fills its
/// box; the video is letterboxed inside it.
#[component]
pub fn VideoPlayer(
  /// The decrypted file's `blob:` URL.
  src: String,
  /// The video element, for the caller's own control (the lightbox's play key, pausing out of view).
  video: NodeRef<html::Video>,
  /// What the player is called for screen readers (the file name).
  #[prop(into)]
  label: Signal<String>,
  /// The player's class: how it sits in its place.
  class: &'static str,
  /// Starts playing once loaded (the user just pressed play on the tile).
  #[prop(optional)]
  is_autoplay: bool,
  /// The first frame's size is known.
  #[prop(optional)]
  maybe_on_loaded: Option<Callback<()>>,
  /// The video can't be played here: the webview reported an error, or the video gave no sign of
  /// playing within [`STALL_TIMEOUT`]. Runs at most once.
  on_unplayable: Callback<()>,
) -> impl IntoView {
  let i18n = use_i18n();
  let player = NodeRef::<html::Div>::new();
  let timeline = Timeline::default();
  let is_paused = RwSignal::new(true);
  let is_muted = RwSignal::new(false);
  let volume = RwSignal::new(1.0_f64);
  let is_fullscreen = RwSignal::new(false);
  let speed = RwSignal::new(1.0_f64);
  let is_repeating = RwSignal::new(false);
  let maybe_pointer_at = RwSignal::new(None::<PointerTime>);
  let idle = IdleWatch::default();
  on_cleanup(move || idle.stop());
  let can_fullscreen = document().fullscreen_enabled();
  let controls = Controls {
    video,
    player,
    is_fullscreen,
  };
  // Counted from here, when the file's `blob:` URL is set: never while it still downloads.
  let stalls = StallWatch::new(video, on_unplayable);
  stalls.watch(StallCheck::Metadata);
  on_cleanup(move || stalls.stop());

  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    // Space and Enter on a button press it; the player doesn't press it twice.
    let is_button = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::HtmlButtonElement>().ok())
      .is_some();
    if is_button && matches!(ev.key().as_str(), " " | "Enter") {
      return;
    }
    let has_modifier = ev.ctrl_key() || ev.meta_key() || ev.alt_key();
    let Some(command) = PlayerCommand::for_key(&ev.key(), has_modifier) else {
      return;
    };
    // Arrows seek here instead of paging the lightbox, and the scrubber steps the same way.
    ev.prevent_default();
    ev.stop_propagation();
    controls.run(command);
  };
  let on_seek = move |ev: ev::Event| {
    if let Ok(seconds) = event_target_value(&ev).parse::<f64>() {
      controls.seek_to(seconds);
    }
  };
  let on_scrubber_hover = move |ev: web_sys::MouseEvent| {
    let scrubber = event_target::<web_sys::HtmlInputElement>(&ev);
    let rect = scrubber.get_bounding_client_rect();
    let pointer_x = f64::from(ev.client_x()) - rect.left();
    let maybe_seconds = scrubber_time(pointer_x, rect.width(), timeline.duration.get_untracked());
    maybe_pointer_at.set(maybe_seconds.map(|seconds| PointerTime { pointer_x, seconds }));
  };
  let on_speed = move |_| {
    let Some(element) = video.get_untracked() else {
      return;
    };
    let next = next_speed(speed.get_untracked());
    element.set_playback_rate(next);
    speed.set(next);
  };
  let on_repeat = move |_| {
    let Some(element) = video.get_untracked() else {
      return;
    };
    let next = !is_repeating.get_untracked();
    element.set_loop(next);
    is_repeating.set(next);
  };
  let speed_label = move || i18n.t_with("video-speed", &[("speed", &format_speed(speed.get()))]);
  let on_volume = move |ev: ev::Event| {
    if let Ok(level) = event_target_value(&ev).parse::<f64>() {
      controls.set_volume(level);
    }
  };
  let position_text = move || {
    i18n.t_with(
      "video-position",
      &[
        ("current", &format_seconds(timeline.current.get())),
        ("total", &format_seconds(timeline.duration.get())),
      ],
    )
  };
  let play_label = move || match is_paused.get() {
    true => i18n.t("video-play-button"),
    false => i18n.t("video-pause"),
  };

  view! {
    <div
      class=format!("video-player {class}")
      class:is-playing=move || !is_paused.get()
      class:is-fullscreen=move || is_fullscreen.get()
      class:is-idle=move || idle.is_idle.get()
      role="group"
      aria-label=label
      node_ref=player
      on:keydown=on_keydown
      on:mousemove=move |_| idle.wake()
      on:fullscreenchange=move |_| is_fullscreen.set_if_alive(document().fullscreen_element().is_some())
    >
      <video
        class="video-player-video"
        src=src
        playsinline=""
        preload="metadata"
        prop:autoplay=is_autoplay
        node_ref=video
        on:loadedmetadata=move |ev| {
          let element = event_target::<web_sys::HtmlVideoElement>(&ev);
          timeline.update(&element);
          if playback_state(&element).lacks_picture() {
            tracing::info!("a video has no picture size: the webview can't decode it");
            stalls.report();
            return;
          }
          if let Some(on_loaded) = maybe_on_loaded {
            on_loaded.run(());
          }
        }
        on:durationchange=move |ev| timeline.update(&event_target::<web_sys::HtmlVideoElement>(&ev))
        on:timeupdate=move |ev| timeline.update(&event_target::<web_sys::HtmlVideoElement>(&ev))
        on:progress=move |ev| timeline.update(&event_target::<web_sys::HtmlVideoElement>(&ev))
        on:play=move |ev| {
          is_paused.set_if_alive(false);
          idle.wake();
          let from_seconds = event_target::<web_sys::HtmlVideoElement>(&ev).current_time();
          stalls.watch(StallCheck::Start { from_seconds });
        }
        on:pause=move |_| is_paused.set_if_alive(true)
        on:ended=move |_| is_paused.set_if_alive(true)
        on:volumechange=move |ev| {
          let element = event_target::<web_sys::HtmlVideoElement>(&ev);
          volume.set_if_alive(element.volume());
          is_muted.set_if_alive(element.muted());
        }
        on:click=move |_| controls.run(PlayerCommand::TogglePlay)
        on:dblclick=move |_| {
          if can_fullscreen {
            controls.run(PlayerCommand::ToggleFullscreen);
          }
        }
        on:error=move |_| stalls.report()
      ></video>
      // The large button over a paused video; the bar's own button is the one for the keyboard.
      <button
        class="video-big-play"
        class:is-hidden=move || !is_paused.get()
        tabindex="-1"
        aria-hidden="true"
        on:click=move |_| controls.run(PlayerCommand::TogglePlay)
      >
        <Icon name=IconName::Play />
      </button>
      <div class="video-controls">
        <button class="video-btn" aria-label=play_label title=play_label on:click=move |_| controls.run(PlayerCommand::TogglePlay)>
          <Icon name=Signal::derive(move || if is_paused.get() { IconName::Play } else { IconName::Pause }) />
        </button>
        <span class="video-time" aria-hidden="true">{move || format_seconds(timeline.current.get())}</span>
        <div class="video-scrub">
          <input
            class="video-scrubber"
            type="range"
            min="0"
            step="any"
            prop:max=move || timeline.max()
            prop:value=move || timeline.current.get()
            style:--media-played=move || format!("{:.2}%", percent_of(timeline.current.get(), timeline.duration.get()))
            style:--media-buffered=move || format!("{:.2}%", percent_of(timeline.buffered.get(), timeline.duration.get()))
            aria-label=move || i18n.t("video-seek")
            aria-valuetext=position_text
            on:input=on_seek
            on:mousemove=on_scrubber_hover
            on:mouseleave=move |_| maybe_pointer_at.set(None)
          />
          {move || maybe_pointer_at.get().map(|at| view! {
            <span class="video-pointer-time" aria-hidden="true" style:left=format!("{:.0}px", at.pointer_x)>
              {format_seconds(at.seconds)}
            </span>
          })}
        </div>
        <span class="video-time video-duration" aria-hidden="true">{move || format_seconds(timeline.duration.get())}</span>
        <button class="video-btn video-speed" aria-label=speed_label title=speed_label on:click=on_speed>
          {move || format_speed(speed.get())}
        </button>
        <button
          class="video-btn video-repeat"
          aria-label=move || i18n.t("video-repeat")
          title=move || i18n.t("video-repeat")
          aria-pressed=move || if is_repeating.get() { "true" } else { "false" }
          on:click=on_repeat
        >
          <Icon name=IconName::Repeat />
        </button>
        <button
          class="video-btn"
          aria-label=move || if is_muted.get() { i18n.t("video-unmute") } else { i18n.t("video-mute") }
          title=move || if is_muted.get() { i18n.t("video-unmute") } else { i18n.t("video-mute") }
          on:click=move |_| controls.run(PlayerCommand::ToggleMute)
        >
          <Icon name=Signal::derive(move || if is_muted.get() || volume.get() == 0.0 { IconName::VolumeOff } else { IconName::Volume }) />
        </button>
        <input
          class="video-volume"
          type="range"
          min="0"
          max="1"
          step="0.05"
          prop:value=move || if is_muted.get() { 0.0 } else { volume.get() }
          style:--media-played=move || format!("{:.0}%", if is_muted.get() { 0.0 } else { volume.get() * 100.0 })
          aria-label=move || i18n.t("video-volume")
          on:input=on_volume
        />
        {can_fullscreen.then(|| {
          let label = move || match is_fullscreen.get() {
            true => i18n.t("video-exit-fullscreen"),
            false => i18n.t("video-fullscreen"),
          };
          view! {
            <button class="video-btn" aria-label=label title=label on:click=move |_| controls.run(PlayerCommand::ToggleFullscreen)>
              <Icon name=Signal::derive(move || if is_fullscreen.get() { IconName::Minimize } else { IconName::Maximize }) />
            </button>
          }
        })}
      </div>
    </div>
  }
}

/// Where playback stands, as the video element last reported it (seconds; the duration is NaN
/// until it is known).
#[derive(Clone, Copy)]
struct Timeline {
  current: RwSignal<f64>,
  duration: RwSignal<f64>,
  /// The end of the buffered stretch the position is in.
  buffered: RwSignal<f64>,
}

/// Where the pointer is on the scrubber and the time it stands for.
#[derive(Clone, Copy)]
struct PointerTime {
  /// From the scrubber's left edge, in CSS pixels.
  pointer_x: f64,
  seconds: f64,
}

/// Whether the pointer rested long enough for the controls of a playing video to go. Only a timer
/// while the pointer has just moved, so a video nobody touches costs nothing.
#[derive(Clone, Copy)]
struct IdleWatch {
  is_idle: RwSignal<bool>,
  maybe_timer: StoredValue<Option<TimeoutHandle>>,
}

/// Timers that tell an unplayable video from one that plays (#804).
#[derive(Clone, Copy)]
struct StallWatch {
  video: NodeRef<html::Video>,
  on_unplayable: Callback<()>,
  maybe_metadata_timer: StoredValue<Option<TimeoutHandle>>,
  maybe_start_timer: StoredValue<Option<TimeoutHandle>>,
  is_reported: StoredValue<bool>,
}

/// What the buttons and keys act on.
#[derive(Clone, Copy)]
struct Controls {
  video: NodeRef<html::Video>,
  player: NodeRef<html::Div>,
  is_fullscreen: RwSignal<bool>,
}

impl Default for Timeline {
  fn default() -> Self {
    Self {
      current: RwSignal::new(0.0),
      duration: RwSignal::new(f64::NAN),
      buffered: RwSignal::new(0.0),
    }
  }
}

impl Timeline {
  fn update(self, video: &web_sys::HtmlVideoElement) {
    let current = video.current_time();
    let ranges = video.buffered();
    let spans: Vec<(f64, f64)> = (0..ranges.length())
      .filter_map(|index| Some((ranges.start(index).ok()?, ranges.end(index).ok()?)))
      .collect();
    // A media event can still come in after the player went (a removed video pauses).
    self.current.set_if_alive(current);
    self.duration.set_if_alive(video.duration());
    self
      .buffered
      .set_if_alive(buffered_until(&spans, current).max(current));
  }

  /// The scrubber's end: the duration once it is known.
  fn max(self) -> f64 {
    let duration = self.duration.get();
    if duration.is_finite() { duration } else { 0.0 }
  }
}

impl Default for IdleWatch {
  fn default() -> Self {
    Self {
      is_idle: RwSignal::new(false),
      maybe_timer: StoredValue::new(None),
    }
  }
}

impl IdleWatch {
  /// The pointer moved: the controls come back, and go again after [`CONTROLS_IDLE_TIMEOUT`].
  fn wake(self) {
    self.stop();
    // A media event can still come in after the player went (a removed video pauses).
    if self.is_idle.try_get_untracked() == Some(true) {
      self.is_idle.set_if_alive(false);
    }
    match set_timeout_with_handle(move || self.is_idle.set_if_alive(true), CONTROLS_IDLE_TIMEOUT) {
      Ok(handle) => {
        self.maybe_timer.try_set_value(Some(handle));
      },
      Err(error) => tracing::debug!(?error, "no timer for hiding the video's controls"),
    }
  }

  fn stop(self) {
    if let Some(Some(handle)) = self.maybe_timer.try_get_value() {
      handle.clear();
    }
    self.maybe_timer.try_set_value(None);
  }
}

impl StallWatch {
  fn new(video: NodeRef<html::Video>, on_unplayable: Callback<()>) -> Self {
    Self {
      video,
      on_unplayable,
      maybe_metadata_timer: StoredValue::new(None),
      maybe_start_timer: StoredValue::new(None),
      is_reported: StoredValue::new(false),
    }
  }

  /// Checks `check` after [`STALL_TIMEOUT`], replacing an earlier check of the same kind.
  fn watch(self, check: StallCheck) {
    let slot = match check {
      StallCheck::Metadata => self.maybe_metadata_timer,
      StallCheck::Start { .. } => self.maybe_start_timer,
    };
    let fire = move || {
      // The player may be gone by now; then nothing is left to report on.
      let Some(Some(video)) = self.video.try_get_untracked() else {
        return;
      };
      if check.is_stalled(playback_state(&video)) {
        tracing::info!(?check, "a video gave no sign of playing; it can't be played here");
        self.report();
      }
    };
    match set_timeout_with_handle(fire, STALL_TIMEOUT) {
      Ok(handle) => {
        if let Some(Some(earlier)) = slot.try_get_value() {
          earlier.clear();
        }
        slot.try_set_value(Some(handle));
      },
      Err(error) => tracing::debug!(?error, "no timer for the video's stall check"),
    }
  }

  /// Tells the caller once; the timers stop.
  fn report(self) {
    if self.is_reported.try_get_value() != Some(false) {
      return;
    }
    self.is_reported.try_set_value(true);
    self.stop();
    self.on_unplayable.run(());
  }

  fn stop(self) {
    for slot in [self.maybe_metadata_timer, self.maybe_start_timer] {
      if let Some(Some(handle)) = slot.try_get_value() {
        handle.clear();
      }
      slot.try_set_value(None);
    }
  }
}

/// What the stall checks look at.
fn playback_state(video: &web_sys::HtmlVideoElement) -> PlaybackState {
  let ready_state = video.ready_state();
  PlaybackState {
    has_metadata: ready_state >= HAVE_METADATA,
    has_frame: ready_state >= HAVE_CURRENT_DATA,
    has_picture: video.video_width() > 0,
    is_paused: video.paused(),
    is_ended: video.ended(),
    is_seeking: video.seeking(),
    current_seconds: video.current_time(),
  }
}

impl Controls {
  fn run(self, command: PlayerCommand) {
    if command == PlayerCommand::ToggleFullscreen {
      self.toggle_fullscreen();
    } else if let Some(video) = self.video.get_untracked() {
      run_player_command(&video, command);
    }
  }

  fn seek_to(self, seconds: f64) {
    if let Some(video) = self.video.get_untracked() {
      video.set_current_time(seconds);
    }
  }

  fn set_volume(self, level: f64) {
    if let Some(video) = self.video.get_untracked() {
      set_video_volume(&video, level);
    }
  }

  /// The whole player goes full screen, so the app's controls come along.
  fn toggle_fullscreen(self) {
    let document = document();
    if document.fullscreen_element().is_some() {
      document.exit_fullscreen();
      return;
    }
    let Some(player) = self.player.get_untracked() else {
      return;
    };
    if let Err(error) = player.request_fullscreen() {
      // Refused (no user gesture, or the webview doesn't allow it): the player stays as it is.
      tracing::debug!(?error, "full screen refused");
      return;
    }
    self.is_fullscreen.set(true);
  }
}

/// Runs a player key on the video; also for the lightbox, which sends them on while the focus is
/// outside the player. Full screen needs the whole player, so it is [`Controls`]'s.
pub fn run_player_command(video: &web_sys::HtmlVideoElement, command: PlayerCommand) {
  match command {
    PlayerCommand::TogglePlay => {
      if video.paused() {
        // A refused play (no user gesture yet) leaves the video paused; the button stays "play".
        drop(video.play());
      } else {
        drop(video.pause());
      }
    },
    PlayerCommand::SeekBy(seconds) => {
      video.set_current_time(seek_target(video.current_time(), seconds, video.duration()));
    },
    PlayerCommand::SeekToStart => video.set_current_time(0.0),
    PlayerCommand::SeekToEnd => {
      let duration = video.duration();
      if duration.is_finite() {
        video.set_current_time(duration);
      }
    },
    PlayerCommand::SeekToTenth(tenths) => {
      if let Some(target) = tenth_target(tenths, video.duration()) {
        video.set_current_time(target);
      }
    },
    PlayerCommand::StepFrame(seconds) => {
      // A refused pause leaves it playing; the step still happens.
      drop(video.pause());
      video.set_current_time(seek_target(video.current_time(), seconds, video.duration()));
    },
    PlayerCommand::VolumeBy(delta) => set_video_volume(video, volume_target(video.volume(), delta)),
    // Unmuted at zero volume would still be silent.
    PlayerCommand::ToggleMute if video.muted() && video.volume() == 0.0 => {
      set_video_volume(video, UNMUTED_VOLUME)
    },
    PlayerCommand::ToggleMute => video.set_muted(!video.muted()),
    PlayerCommand::ToggleFullscreen => {},
  }
}

/// Sets the volume; turning it up unmutes.
fn set_video_volume(video: &web_sys::HtmlVideoElement, level: f64) {
  video.set_volume(level);
  video.set_muted(level == 0.0);
}
