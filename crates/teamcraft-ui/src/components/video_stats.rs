//! "Stats for nerds" (#938) on the call screen: one toggle in the call's controls shows a compact
//! overlay on every video, updated every second, with a button that copies it for bug reports.
//! The rows come from [`crate::video_stats`]; the media layer's reports from the backend
//! ([`teamcraft_client_api::backend::CallBackend::fetch_call_video_stats`]).

use leptos::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::reactive::{use_interval_while, use_page_visible};
use crate::state::{AppState, use_app_state};
use crate::video_stats::{DrawnRate, StatsTarget, StatsWindow, TileMeasure, stats_rows, stats_text};

/// How often the media layer is asked, and the overlay updated.
const STATS_INTERVAL_MS: u64 = 1_000;

/// The call screen's stats: whether they show, and the media layer's last two reports.
#[derive(Clone, Copy)]
pub struct VideoStatsHub {
  pub is_on: RwSignal<bool>,
  window: RwSignal<StatsWindow>,
}

impl VideoStatsHub {
  /// Stats that show while `is_on`: the media layer is asked every second then (while the window
  /// is visible), and not at all otherwise. Provided as context for the tiles' overlays.
  pub fn provide(is_on: RwSignal<bool>) -> Self {
    let state = use_app_state();
    let is_page_visible = use_page_visible();
    let hub = Self::new(is_on);
    use_interval_while(
      STATS_INTERVAL_MS,
      move || is_on.get() && is_page_visible.get(),
      move || hub.poll(state),
    );
    provide_context(hub);
    hub
  }

  fn new(is_on: RwSignal<bool>) -> Self {
    Self {
      is_on,
      window: RwSignal::new(StatsWindow::default()),
    }
  }

  /// Shows or hides the stats. Hidden, they forget their reports: shown again later, they show
  /// no rate across the pause.
  pub fn toggle(&self) {
    let shows = !self.is_on.get_untracked();
    if !shows {
      self.window.update(StatsWindow::clear);
    }
    self.is_on.set(shows);
  }

  /// Asks the media layer for a report; outside a call (none) the overlays wait.
  fn poll(&self, state: AppState) {
    let window = self.window;
    state.read(|backend| {
      backend.fetch_call_video_stats(Box::new(move |maybe_report| {
        // The call screen may have gone meanwhile.
        let _ = window.try_update(|window| match maybe_report {
          Some(report) => window.push(report),
          None => window.clear(),
        });
      }));
    });
  }
}

/// The overlay on one video while the stats show. `maybe_host` is the element the video is drawn
/// into; for another device's video the tile measures how often it draws and how large.
#[component]
pub fn VideoStatsOverlay(
  target: StatsTarget,
  #[prop(optional)] maybe_host: Option<NodeRef<leptos::html::Div>>,
) -> impl IntoView {
  let Some(hub) = use_context::<VideoStatsHub>() else {
    return ().into_any();
  };
  // The own video's numbers all come from the media layer.
  let maybe_host = maybe_host.filter(|_| matches!(target, StatsTarget::Received(_)));
  let i18n = use_i18n();
  let state = use_app_state();
  let measure = RwSignal::new(TileMeasure::default());
  let drawn = StoredValue::new(DrawnRate::default());
  // Measured with every report, so the drawn rate covers the same second.
  Effect::new(move |_| {
    hub.window.track();
    if !hub.is_on.get_untracked() {
      return;
    }
    let Some(host) = maybe_host.and_then(|host| host.get_untracked()) else {
      return;
    };
    let (maybe_count, maybe_drawn_size) = measure_drawn(&host);
    let maybe_drawn_fps = maybe_count.and_then(|count| {
      drawn
        .try_update_value(|rate| {
          rate.observe(count, crate::motion::now());
          rate.fps()
        })
        .flatten()
    });
    measure.set(TileMeasure {
      maybe_drawn_fps,
      maybe_drawn_size,
    });
  });
  let title = move || match target {
    StatsTarget::OwnCamera => i18n.t("stats-sent-camera"),
    StatsTarget::OwnScreen => i18n.t("stats-sent-screen"),
    StatsTarget::Received(stream_id) => {
      i18n.t_with("stats-received", &[("stream", &stream_id.0.to_string())])
    },
  };
  let rows = Memo::new(move |_| {
    let words = |key: &str, args: &[(&str, &str)]| i18n.t_with(key, args);
    hub
      .window
      .with(|window| stats_rows(window, target, measure.get(), &words))
  });
  let copy = move |_| {
    let Some(rows) = rows.get_untracked() else {
      return;
    };
    let heading = format!("{} · {}", i18n.t("stats-title"), title());
    let text = stats_text(&heading, &rows, |key| i18n.t(key));
    // Started inside the click, where browsers allow it.
    let copied = crate::components::message_actions::copy_text(&text);
    leptos::task::spawn_local(async move {
      if copied.await && state.is_alive() {
        state.show_toast(i18n.t("stats-copied"));
      }
    });
  };
  view! {
    <Show when=move || hub.is_on.get() fallback=|| ()>
      <div class="video-stats" role="group" aria-label=move || i18n.t("stats-title")>
        <div class="video-stats-head">
          <span class="video-stats-title">{title}</span>
          <button
            type="button"
            class="video-stats-copy"
            aria-label=move || i18n.t("stats-copy")
            title=move || i18n.t("stats-copy")
            disabled=move || rows.with(Option::is_none)
            on:click=copy
          >
            <Icon name=IconName::Copy />
          </button>
        </div>
        {move || match rows.get() {
          Some(rows) => view! {
            <dl class="video-stats-rows">
              {rows
                .into_iter()
                .map(|row| view! { <dt>{i18n.t(row.label_key)}</dt> <dd translate="no">{row.value}</dd> })
                .collect_view()}
            </dl>
          }
          .into_any(),
          None => view! { <p class="video-stats-waiting">{i18n.t("stats-waiting")}</p> }.into_any(),
        }}
      </div>
    </Show>
  }
  .into_any()
}

/// The pictures drawn into the element `host` shows, and its picture size (a canvas' or a
/// video's own size, not the size it is shown at).
#[cfg(target_arch = "wasm32")]
fn measure_drawn(
  host: &web_sys::HtmlDivElement,
) -> (Option<u64>, Option<teamcraft_client_api::video_stats::VideoSize>) {
  use js_sys::Reflect;
  use teamcraft_client_api::video_stats::VideoSize;
  use wasm_bindgen::JsValue;

  let Some(element) = host.first_element_child() else {
    return (None, None);
  };
  let maybe_count = teamcraft_media::web_api::pictures_drawn(&element);
  let side = |name: &str| {
    Reflect::get(&element, &JsValue::from_str(name))
      .ok()
      .and_then(|value| value.as_f64())
      .filter(|pixels| *pixels >= 1.0)
      // Saturating: a picture side is far below u16::MAX.
      .map(|pixels| pixels as u16)
  };
  let maybe_size = side("videoWidth")
    .zip(side("videoHeight"))
    .or_else(|| side("width").zip(side("height")))
    .map(|(width, height)| VideoSize { width, height });
  (maybe_count, maybe_size)
}

#[cfg(not(target_arch = "wasm32"))]
fn measure_drawn(
  _host: &web_sys::HtmlDivElement,
) -> (Option<u64>, Option<teamcraft_client_api::video_stats::VideoSize>) {
  (None, None)
}

#[cfg(test)]
mod tests {
  use std::rc::Rc;

  use teamcraft_client_api::video_stats::{ReceivedVideoStats, VideoStatsReport, VideoTrackStats};
  use teamcraft_proto::{MediaStreamId, VideoCodec, VideoTrackKind};

  use super::*;
  use crate::backend::Backend;
  use crate::mock::MockBackend;
  use crate::settings::Settings;
  use crate::state::AppState;

  const STREAM: MediaStreamId = MediaStreamId(4);

  /// The hub's reports give the tiles' rows; a backend that reports nothing (no call, or one
  /// without stats) leaves them waiting rather than showing old numbers, and so does turning the
  /// stats off and on.
  #[test]
  fn the_hub_feeds_the_tiles_and_forgets_what_is_stale() {
    let owner = Owner::new();
    owner.with(|| {
      let state = AppState::new(
        Rc::new(MockBackend::new(0)) as Rc<dyn Backend>,
        Settings::for_tests(),
        None,
      );
      let is_on = RwSignal::new(true);
      let hub = VideoStatsHub::new(is_on);
      let feed = || {
        hub.window.update(|window| {
          window.push(report(1_000, 0));
          window.push(report(2_000, 125_000));
        });
      };
      let bitrate = || {
        hub.window.with_untracked(|window| {
          let words = |key: &str, _: &[(&str, &str)]| key.to_owned();
          stats_rows(
            window,
            StatsTarget::Received(STREAM),
            TileMeasure::default(),
            &words,
          )
          .and_then(|rows| rows.into_iter().find(|row| row.label_key == "stats-bitrate"))
          .map(|row| row.value)
        })
      };
      feed();
      assert_eq!(bitrate().as_deref(), Some("1.00 Mbit/s"));

      // The demo backend has no media layer: it answers at once with nothing.
      hub.poll(state);
      assert_eq!(bitrate(), None);

      feed();
      hub.toggle();
      assert!(!is_on.get_untracked());
      assert_eq!(hub.window.get_untracked(), StatsWindow::default());
      hub.toggle();
      assert!(is_on.get_untracked());
      assert_eq!(bitrate(), None, "no numbers from before the pause");
    });
  }

  fn report(at_ms: u64, bytes: u64) -> VideoStatsReport {
    VideoStatsReport {
      at_ms,
      tracks: vec![VideoTrackStats::Received(ReceivedVideoStats {
        stream_id: STREAM,
        kind: VideoTrackKind::Camera,
        codec: VideoCodec::H264,
        layers: Vec::new(),
        maybe_layer: None,
        maybe_max_layer: None,
        packets: bytes / 1000,
        bytes,
        packets_lost: 0,
        frames: bytes / 4000,
        frames_discarded: 0,
        maybe_jitter_ms: None,
      })],
      ..VideoStatsReport::default()
    }
  }
}
