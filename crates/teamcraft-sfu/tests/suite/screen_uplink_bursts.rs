//! A screen share's bursts on a limited uplink (#1005): a slide change or a keyframe of a sharp
//! text screen is far larger than a frame's share of the layer's bitrate (VideoToolbox: 5 to 28
//! times, docs/performance.md, "Short bitrate peaks of the encoders"). Do they hold the picture up
//! or get lost on the way to the SFU (#1205)? `screen_bench`'s model link answers it for one
//! layer; this goes through the real call, SFU and estimator.
//!
//! The presenter shares slides of text (a full change every [`SLIDE_INTERVAL`], nothing between,
//! as ScreenCaptureKit delivers a still screen) through a relay that limits its uplink like a
//! bottleneck (`common::relay`: packets leave at the link's rate, wait at most 250 ms, are
//! dropped beyond), with the desktop's detail layers and encoders (`preferred_encoders`:
//! VideoToolbox with `--features teamcraft-media/gstreamer` on a Mac, OpenH264 otherwise). The
//! viewer asks for the top layer, as the desktop's stage does. Per slide: how long until the
//! viewer shows it (at any size, and sharp at the shared size).
//!
//! `measure_screen_share_bursts_on_a_limited_uplink` (ignored) prints the numbers per uplink:
//! `cargo test --release -p teamcraft-sfu --test suite measure_screen_share_bursts -- --ignored
//! --nocapture` (add `--features teamcraft-media/gstreamer` for VideoToolbox).

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use teamcraft_media::call_engine::CallEngineEvent;
use teamcraft_media::capture_convert::{PixelFormat, RawPicture};
use teamcraft_media::video::{VideoFrame, VideoResolution, screen_layers, screen_limits};
use teamcraft_media::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture};
use teamcraft_media::video_pipeline::{VideoSendConfig, preferred_encoders};
use teamcraft_proto::{ScreenContentMode, ScreenShareResolution, VideoLayerInfo, VideoTrackKind};

use crate::common::relay::Relay;
use crate::screen_sharpness::{
  Encoded, SHARED, counting, fill, join, pump_for, shown_at, start_sfu, text, wait_until_secured,
};

/// How long a slide stays.
const SLIDE_INTERVAL: Duration = Duration::from_secs(2);
/// Distinct slides, shown in turn.
const SLIDES: usize = 3;
/// How long the call carries only sound before the share: the uplink estimate measures the link.
const SOUND_ONLY: Duration = Duration::from_secs(4);
/// How long the share is watched.
const SHARE: Duration = Duration::from_secs(24);
/// The first slides go out while the estimate still grows and the viewer gets its layers: they
/// aren't judged.
const SETTLE: Duration = Duration::from_secs(8);
/// The uplink of the asserting test: room for both layers of the share (8.3 Mbit/s of targets)
/// and its sound, not for a keyframe of both at once within the bottleneck's queue.
const LIMIT_BPS: u64 = 12_000_000;
/// The longest a slide may take to reach the viewer once the share settled: 0.1 s on a laptop
/// with VideoToolbox, 0.35 s with OpenH264 in a debug build; room for a slow CI runner.
const MAX_SLIDE_DELAY: Duration = Duration::from_secs(3);

/// #1005: slides of text reach the viewer through an uplink with room for the share but not for
/// its bursts at once, each within seconds, and the bottleneck drops next to nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slides_reach_the_viewer_through_a_limited_uplink() {
  let run = run(Some(LIMIT_BPS)).await;
  let summary = run.summary();
  println!("{summary}");
  let delays = run.slide_delays();
  assert!(!delays.is_empty(), "no slide was judged:\n{summary}");
  assert!(
    delays.iter().all(|(_, maybe_delay)| maybe_delay.is_some()),
    "a slide never reached the viewer:\n{summary}"
  );
  let worst = delays
    .iter()
    .filter_map(|(_, maybe_delay)| *maybe_delay)
    .max()
    .unwrap();
  assert!(
    worst <= MAX_SLIDE_DELAY,
    "a slide took {worst:?} to reach the viewer:\n{summary}"
  );
  let dropped = run.dropped_share();
  assert!(
    dropped < 0.05,
    "{:.1} % of the uplink's bytes dropped:\n{summary}",
    dropped * 100.0
  );
}

/// Prints per uplink what the share sent and how its slides reached the viewer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement: prints a summary per uplink, takes about two minutes"]
async fn measure_screen_share_bursts_on_a_limited_uplink() {
  for maybe_limit_bps in [
    None,
    Some(20_000_000),
    Some(12_000_000),
    Some(10_000_000),
    Some(4_000_000),
  ] {
    let run = run(maybe_limit_bps).await;
    println!("== uplink {maybe_limit_bps:?} bit/s\n{}", run.summary());
  }
}

/// What a run recorded, timed from the share's start.
struct Run {
  layers: Vec<VideoLayerInfo>,
  /// When each slide went up, and its capture timestamp.
  changes: Vec<(Duration, u64)>,
  /// The viewer's pictures: when, the capture timestamp, the size.
  received: Vec<(Duration, u64, VideoResolution)>,
  shown: BTreeMap<u64, usize>,
  encoded: Vec<Encoded>,
  /// The uplink's packets: when, bytes, whether dropped.
  link: Vec<(Duration, usize, bool)>,
  /// The presenter's uplink estimates: when, bit/s.
  uplink_estimates: Vec<(Duration, u32)>,
  layers_needed: Vec<Vec<u8>>,
  started_at: Duration,
}

async fn run(maybe_limit_bps: Option<u64>) -> Run {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let start = Instant::now();
  let relay = Relay::start(sfu, start);
  relay.to_sfu.set_limit(maybe_limit_bps);
  let mut presenter = join(relay.address, certificate_hash, 1, 2).await;
  let mut viewer = join(sfu, certificate_hash, 2, 1).await;
  wait_until_secured(&mut presenter).await;
  wait_until_secured(&mut viewer).await;
  pump_for(&mut presenter, &mut viewer, SOUND_ONLY).await;

  let slides = SlideShow::new(start);
  let (shown, changes) = (Arc::clone(&slides.shown), Arc::clone(&slides.changes));
  let encoded: Arc<Mutex<Vec<Encoded>>> = Arc::default();
  let (limit, framerate) = screen_limits(ScreenContentMode::Detail, &ScreenShareResolution::Standard);
  let layers = screen_layers(
    limit,
    framerate,
    ScreenContentMode::Detail,
    &ScreenShareResolution::Standard,
  );
  let started_at = start.elapsed();
  let config = VideoSendConfig {
    maybe_layer_plan: None,
    kind: VideoTrackKind::Screen,
    layers: layers.clone(),
    capture: Box::new(slides),
    encoders: counting(preferred_encoders(), start, Arc::clone(&encoded)),
    maybe_filter: None,
    maybe_picture_tap: None,
    maybe_idle_refresh: VideoSendConfig::default_idle_refresh(&VideoTrackKind::Screen),
  };
  presenter.start_video(config).await.unwrap();

  let received: Arc<Mutex<Vec<(Duration, u64, VideoResolution)>>> = Arc::default();
  let (mut uplink_estimates, mut layers_needed) = (Vec::new(), Vec::new());
  let end = tokio::time::Instant::now() + SHARE;
  while tokio::time::Instant::now() < end {
    match tokio::time::timeout(Duration::from_millis(5), presenter.next_event()).await {
      Ok(Some(CallEngineEvent::LayersNeeded { layers, .. })) => layers_needed.push(layers),
      Ok(Some(CallEngineEvent::UplinkEstimate { bps, .. })) => uplink_estimates.push((start.elapsed(), bps)),
      _ => {},
    }
    if let Ok(Some(CallEngineEvent::VideoPublished(track))) =
      tokio::time::timeout(Duration::from_millis(5), viewer.next_event()).await
      && track.kind == VideoTrackKind::Screen
    {
      let sink = Arc::clone(&received);
      viewer.watch_video(
        track.stream_id,
        None,
        Box::new(move |frame| {
          sink
            .lock()
            .unwrap()
            .push((start.elapsed(), frame.timestamp_us(), frame.resolution()));
        }),
      );
    }
  }
  presenter.stop_video(VideoTrackKind::Screen);
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();
  let received = received.lock().unwrap().clone();
  let shown = shown.lock().unwrap().clone();
  let changes = changes.lock().unwrap().clone();
  let encoded = encoded.lock().unwrap().clone();
  let link = relay.to_sfu.log.lock().unwrap().clone();
  Run {
    layers,
    changes,
    received,
    shown,
    encoded,
    link,
    uplink_estimates,
    layers_needed,
    started_at,
  }
}

impl Run {
  /// Per slide after [`SETTLE`]: how long until the viewer showed it at any size, `None` if not
  /// before the next one.
  fn slide_delays(&self) -> Vec<(Duration, Option<Duration>)> {
    self.delays(|_| true)
  }

  /// The same, at the shared size.
  fn sharp_delays(&self) -> Vec<(Duration, Option<Duration>)> {
    self.delays(|resolution| resolution == SHARED)
  }

  fn delays(&self, is_counted: impl Fn(VideoResolution) -> bool) -> Vec<(Duration, Option<Duration>)> {
    let settled = self.started_at + SETTLE;
    let mut delays = Vec::new();
    // The last slide may not have had its time yet.
    for pair in self.changes.windows(2) {
      let [(changed_at, timestamp_us), (_, next_us)] = pair else {
        continue;
      };
      if *changed_at < settled {
        continue;
      }
      let slide = shown_at(&self.shown, *timestamp_us);
      let maybe_delay = self
        .received
        .iter()
        .find(|(_, picture_us, resolution)| {
          *picture_us + 100 >= *timestamp_us
            && *picture_us + 100 < *next_us
            && shown_at(&self.shown, *picture_us) == slide
            && is_counted(*resolution)
        })
        .map(|(at, ..)| at.saturating_sub(*changed_at));
      delays.push((changed_at.saturating_sub(self.started_at), maybe_delay));
    }
    delays
  }

  /// The lowest and highest uplink estimate after [`SETTLE`], bit/s.
  fn estimate_range(&self) -> (u32, u32) {
    let settled = self.started_at + SETTLE;
    let estimates = self
      .uplink_estimates
      .iter()
      .filter(|(at, _)| *at >= settled)
      .map(|(_, bps)| *bps);
    estimates.fold((u32::MAX, 0), |(low, high), bps| (low.min(bps), high.max(bps)))
  }

  /// The share of the uplink's bytes the bottleneck dropped after [`SETTLE`].
  fn dropped_share(&self) -> f64 {
    let settled = self.started_at + SETTLE;
    let (mut all, mut dropped) = (0usize, 0usize);
    for (at, bytes, is_dropped) in &self.link {
      if *at >= settled {
        all += bytes;
        if *is_dropped {
          dropped += bytes;
        }
      }
    }
    dropped as f64 / all.max(1) as f64
  }

  fn summary(&self) -> String {
    let format_delays = |delays: Vec<(Duration, Option<Duration>)>| {
      delays
        .iter()
        .map(|(_, maybe_delay)| {
          maybe_delay.map_or_else(|| "-".to_owned(), |delay| delay.as_millis().to_string())
        })
        .collect::<Vec<_>>()
        .join(" ")
    };
    let mut lines = vec![
      format!(
        "slide delays, any size (ms): {}",
        format_delays(self.slide_delays())
      ),
      format!(
        "slide delays, sharp (ms):    {}",
        format_delays(self.sharp_delays())
      ),
      format!(
        "uplink: {:.1} % of the bytes dropped after the first {} s; estimate {}–{} kbit/s after it; layers needed {:?}",
        self.dropped_share() * 100.0,
        SETTLE.as_secs(),
        self.estimate_range().0 / 1000,
        self.estimate_range().1 / 1000,
        self.layers_needed.last(),
      ),
    ];
    let seconds = (SHARE.as_secs_f64() - 1.0).max(1.0);
    // The layers were planned for the largest picture; the encoders work at the shared screen's
    // size (#931): its layers in the same order.
    let mut widths: Vec<u16> = self.encoded.iter().map(|picture| picture.width).collect();
    widths.sort_unstable();
    widths.dedup();
    let mut layers = self.layers.clone();
    layers.sort_by_key(|layer| layer.width);
    for (layer, width) in layers.iter().zip(widths) {
      let pictures: Vec<&Encoded> = self
        .encoded
        .iter()
        .filter(|picture| picture.width == width)
        .collect();
      let in_ms = |bytes: usize| bytes as f64 * 8.0 / f64::from(layer.max_bitrate_bps) * 1000.0;
      let largest = |want_keyframe: bool| {
        pictures
          .iter()
          .filter(|picture| picture.is_keyframe == want_keyframe)
          .map(|picture| in_ms(picture.bytes))
          .fold(0.0, f64::max)
      };
      lines.push(format!(
        "layer {width} wide at {} kbit/s: {} pictures, {} keyframes, {:.0} kbit/s, largest keyframe {:.0} ms, other {:.0} ms of the bitrate",
        layer.max_bitrate_bps / 1000,
        pictures.len(),
        pictures.iter().filter(|picture| picture.is_keyframe).count(),
        pictures.iter().map(|picture| picture.bytes).sum::<usize>() as f64 * 8.0 / seconds / 1000.0,
        largest(true),
        largest(false),
      ));
    }
    lines.join("\n")
  }
}

/// Slides of text, each a full change, delivered when they change (nothing between, as
/// ScreenCaptureKit does with a still screen).
struct SlideShow {
  sources: Arc<Vec<VideoFrame>>,
  start: Instant,
  /// Which slide went up at which capture timestamp.
  shown: Arc<Mutex<BTreeMap<u64, usize>>>,
  /// When each slide went up (since `start`), and its capture timestamp.
  changes: Arc<Mutex<Vec<(Duration, u64)>>>,
  maybe_running: Option<(std_mpsc::Sender<()>, JoinHandle<()>)>,
}

impl SlideShow {
  fn new(start: Instant) -> Self {
    let sources = (0..SLIDES)
      .map(|slide| {
        let bgrx = draw_slide(slide);
        let picture = RawPicture {
          format: PixelFormat::Bgrx,
          width: u32::from(SHARED.width),
          height: u32::from(SHARED.height),
          stride: 0,
          data: &bgrx,
        };
        picture.to_i420(0).unwrap()
      })
      .collect();
    Self {
      sources: Arc::new(sources),
      start,
      shown: Arc::default(),
      changes: Arc::default(),
      maybe_running: None,
    }
  }
}

impl VideoCapture for SlideShow {
  /// As ScreenCaptureKit's capture: the largest picture asked for, not the display's size.
  fn format(&self) -> CaptureFormat {
    let (resolution, framerate) = screen_limits(ScreenContentMode::Detail, &ScreenShareResolution::Standard);
    CaptureFormat {
      resolution,
      framerate,
    }
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    let (stop, stopped) = std_mpsc::channel::<()>();
    let (sources, shown, changes) = (
      Arc::clone(&self.sources),
      Arc::clone(&self.shown),
      Arc::clone(&self.changes),
    );
    let start = self.start;
    let thread = std::thread::spawn(move || {
      let capture_start = Instant::now();
      for index in 0usize.. {
        let slide = index % SLIDES;
        let timestamp_us = u64::try_from(capture_start.elapsed().as_micros()).unwrap();
        shown.lock().unwrap().insert(timestamp_us, slide);
        changes.lock().unwrap().push((start.elapsed(), timestamp_us));
        if !frames.send(sources[slide].clone().with_timestamp_us(timestamp_us)) {
          return;
        }
        if !matches!(
          stopped.recv_timeout(SLIDE_INTERVAL),
          Err(std_mpsc::RecvTimeoutError::Timeout)
        ) {
          return;
        }
      }
    });
    self.maybe_running = Some((stop, thread));
    Ok(())
  }

  fn stop(&mut self) {
    if let Some((stop, thread)) = self.maybe_running.take() {
      drop(stop);
      thread.join().unwrap();
    }
  }
}

/// Packed BGRX of a slide: a title bar, a large heading, bullet points and small print, each
/// slide with other text in other places.
fn draw_slide(slide: usize) -> Vec<u8> {
  let (width, height) = (usize::from(SHARED.width), usize::from(SHARED.height));
  let mut page = vec![[250u8, 250, 250]; width * height];
  fill(&mut page, width, (0, 0, width, 76), [222, 222, 226]);
  let seed = 1000 * (slide + 1);
  let indent = 120 + slide * 40;
  text(&mut page, width, (indent, 140), 6, 30, [20, 40, 120], seed);
  for row in 0..12 {
    text(
      &mut page,
      width,
      (indent + 60, 300 + row * 60),
      3,
      80,
      [25, 25, 30],
      seed + 1 + row,
    );
  }
  for row in 0..15 {
    text(
      &mut page,
      width,
      (indent + 60, 1030 + row * 24),
      2,
      150,
      [70, 70, 80],
      seed + 100 + row,
    );
  }
  page.into_iter().flat_map(|[r, g, b]| [b, g, r, 255]).collect()
}
