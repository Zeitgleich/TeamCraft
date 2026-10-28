//! A camera call through links of limited bandwidth (issue #939): the picture follows the
//! bandwidth down and back up and never stalls for long.
//!
//! Each client reaches a real SFU through a relay that can shape its uplink or downlink like a
//! bottleneck: packets leave at the link's rate, wait in a queue of at most [`MAX_QUEUE_DELAY`]
//! and are dropped when it is full. The presenter sends a busy 720p camera (moving shapes and
//! sensor noise, so OpenH264 uses its whole bitrate) with the desktop's layers.
//!
//! - Uplink: the presenter's uplink is limited to 500 kbit/s, then 2 Mbit/s, then not at all.
//!   The viewer's picture drops to the middle layer (not the smallest: the top sent layer gives
//!   up bitrate before resolution), never stalls for long, and is back at 720p once the uplink
//!   carries it.
//! - Downlink: one of two viewers gets 500 kbit/s, then no limit. The SFU gives it a lower layer
//!   and the top one again once the limit is lifted; the other viewer keeps 720p throughout.
//!
//! The thresholds leave room for slow CI runners (debug builds, other tests in parallel): the
//! measurements on a laptop (ARCHITECTURE.md §6.5) are well within them.
//!
//! `measure_camera_under_bandwidth_limits` (ignored) prints the timeline per second of both:
//! the link's limit, what the viewers decoded (pictures, size, PSNR, longest gap), what the
//! presenter encoded per layer, the layers it sent and its uplink estimate.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use teamcraft_media::audio_io::AudioSource;
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_media::video::{VideoFrame, VideoResolution, simulcast_layers};
use teamcraft_media::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture};
use teamcraft_media::video_codec::{EncodedPicture, VideoCodecError, VideoEncoder};
use teamcraft_media::video_pipeline::{EncoderFactory, VideoSendConfig, software_encoders};
use teamcraft_proto::{AudioCodec, DeviceId, MediaCapabilities, VideoCodec, VideoTrackKind};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use wtransport::Identity;

/// The desktop's camera: 720p at 30 fps, three layers from 1.5 Mbit/s (apps/desktop's
/// `camera_layers`).
const HD: VideoResolution = VideoResolution {
  width: 1280,
  height: 720,
};
const FRAMERATE: u8 = 30;
const CAMERA_BITRATE_BPS: u32 = 1_500_000;
/// Distinct pictures the camera cycles through (one second of motion).
const CAMERA_PICTURES: usize = 30;
/// The bottleneck's queue: a packet that would wait longer is dropped (a home router's buffer).
const MAX_QUEUE_DELAY: Duration = Duration::from_millis(250);
/// The limits of the uplink test, in order; `None` lifts the limit.
const UPLINK_STEPS: [(Option<u64>, Duration); 4] = [
  (None, Duration::from_secs(6)),
  (Some(500_000), Duration::from_secs(10)),
  (Some(2_000_000), Duration::from_secs(10)),
  (None, Duration::from_secs(10)),
];
/// The limits of the limited viewer's downlink, in order.
const DOWNLINK_STEPS: [(Option<u64>, Duration); 3] = [
  (None, Duration::from_secs(6)),
  (Some(500_000), Duration::from_secs(10)),
  (None, Duration::from_secs(14)),
];
/// The longest the picture may stand still, also when a limit sets in: under 1 s measured on a
/// laptop, up to 1.4 s with eight copies of the tests in parallel in a debug build. Before #939, a
/// viewer whose downlink dropped stood still for 2.2 to 3.4 s.
const MAX_STALL: Duration = Duration::from_secs(2);
/// How long after a limit sets in the layers may still be settling.
const SETTLE: Duration = Duration::from_secs(3);
/// The last stretch of a run, by when the top layer has to be back.
const FINAL_STRETCH: Duration = Duration::from_secs(3);
/// Bounds the steps before the camera starts (securing).
const STEP_DEADLINE: Duration = Duration::from_secs(10);

/// The presenter's uplink is limited to 500 kbit/s, then 2 Mbit/s, then not at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_follows_a_limited_uplink_down_and_back_up() {
  let timeline = run(Limited::PresenterUplink, &UPLINK_STEPS).await;
  let summary = timeline.summary();
  println!("{summary}");
  let viewer = 0;
  let limited = timeline.steps[1].0;
  let lifted = timeline.steps[2].0;
  assert!(
    timeline.share_of_hd(viewer, Duration::ZERO, limited) > 0.0,
    "the top layer never reached the viewer before the limit:\n{summary}"
  );
  timeline.assert_no_stall(viewer, &summary);
  // Down, but not to the smallest layer: the top sent layer gives up bitrate first.
  let settled = limited + SETTLE;
  let hd = timeline.share_of_hd(viewer, settled, lifted);
  assert!(
    hd < 0.34,
    "{hd:.2} of the pictures at 500 kbit/s are 720p:\n{summary}"
  );
  let middle = timeline.share_at_least(viewer, 640, settled, lifted);
  assert!(
    middle >= 0.34,
    "only {middle:.2} of the pictures at 500 kbit/s are 640x360 or more:\n{summary}"
  );
  let dropped = timeline.dropped_share(settled, lifted);
  assert!(
    dropped < 0.1,
    "{dropped:.2} of the packets dropped at 500 kbit/s:\n{summary}"
  );
  timeline.assert_back_to_hd(viewer, &summary);
}

/// One of two viewers gets 500 kbit/s, then no limit; the other one is unaffected.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_limited_viewer_gets_a_lower_layer_and_the_other_viewer_keeps_the_top_one() {
  let timeline = run(Limited::ViewerDownlink, &DOWNLINK_STEPS).await;
  let summary = timeline.summary();
  println!("{summary}");
  let (limited_viewer, other_viewer) = (0, 1);
  let limited = timeline.steps[1].0;
  let lifted = timeline.steps[2].0;
  for viewer in [limited_viewer, other_viewer] {
    assert!(
      timeline.share_of_hd(viewer, Duration::ZERO, limited) > 0.0,
      "the top layer never reached viewer {viewer} before the limit:\n{summary}"
    );
    timeline.assert_no_stall(viewer, &summary);
  }
  let hd = timeline.share_of_hd(limited_viewer, limited + SETTLE, lifted);
  assert!(
    hd < 0.34,
    "{hd:.2} of the limited viewer's pictures at 500 kbit/s are 720p:\n{summary}"
  );
  let dropped = timeline.dropped_share(limited + SETTLE, lifted);
  assert!(
    dropped < 0.1,
    "{dropped:.2} of the packets dropped at 500 kbit/s:\n{summary}"
  );
  timeline.assert_back_to_hd(limited_viewer, &summary);
  let first_hd = timeline.decoded[other_viewer]
    .iter()
    .find(|picture| picture.resolution == HD)
    .map_or(Duration::MAX, |picture| picture.at);
  let other = timeline.share_of_hd(other_viewer, first_hd, timeline.end());
  assert!(
    other > 0.95,
    "the other viewer got {other:.2} of its pictures at 720p:\n{summary}"
  );
}

/// Prints the timelines of the uplink and the downlink test.
/// `cargo test --release -p teamcraft-sfu --test suite measure_camera_under_bandwidth_limits --
/// --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement: prints timelines, takes about a minute and a half"]
async fn measure_camera_under_bandwidth_limits() {
  let uplink = run(Limited::PresenterUplink, &UPLINK_STEPS).await;
  println!("== presenter uplink limited\n{}", uplink.summary());
  let downlink = run(Limited::ViewerDownlink, &DOWNLINK_STEPS).await;
  println!("== first viewer's downlink limited\n{}", downlink.summary());
}

/// Which link the steps limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Limited {
  PresenterUplink,
  /// The first of two viewers' downlink.
  ViewerDownlink,
}

/// What happened, timed from the start of the run.
#[derive(Debug, Default)]
struct Timeline {
  /// When each step's limit took effect.
  steps: Vec<(Duration, Option<u64>)>,
  /// When the last step ended.
  maybe_end: Option<Duration>,
  /// Per viewer, what it decoded.
  decoded: Vec<Vec<Decoded>>,
  encoded: Vec<Encoded>,
  /// What the presenter heard: the layers it encodes, its uplink estimate.
  heard: Vec<(Duration, Heard)>,
  /// The limited link: (when, bytes, whether dropped).
  link: Vec<(Duration, usize, bool)>,
}

#[derive(Debug, Clone)]
enum Heard {
  LayersNeeded(Vec<u8>),
  LayerBitrates(Vec<(u8, u32)>),
  UplinkEstimate(u32),
}

#[derive(Debug, Clone, Copy)]
struct Decoded {
  at: Duration,
  resolution: VideoResolution,
  /// Luma PSNR against the camera's picture at its size (a lower layer scaled up), for every
  /// fifth picture.
  maybe_psnr_db: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct Encoded {
  at: Duration,
  width: u16,
  bytes: usize,
  is_keyframe: bool,
}

/// Runs a camera call with `limited` following `steps`.
async fn run(limited: Limited, steps: &[(Option<u64>, Duration)]) -> Timeline {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let start = Instant::now();
  let relay = Relay::start(sfu, start).await;
  let (presenter_address, first_viewer_address) = match limited {
    Limited::PresenterUplink => (relay.address, sfu),
    Limited::ViewerDownlink => (sfu, relay.address),
  };
  let link = match limited {
    Limited::PresenterUplink => relay.to_sfu.clone(),
    Limited::ViewerDownlink => relay.to_client.clone(),
  };
  let viewer_count = match limited {
    Limited::PresenterUplink => 1,
    Limited::ViewerDownlink => 2,
  };
  let others: Vec<i64> = (2..2 + viewer_count).collect();
  let mut presenter = join(presenter_address, certificate_hash, 1, &others).await;
  let mut viewers = Vec::new();
  for (index, device) in others.iter().enumerate() {
    let address = if index == 0 { first_viewer_address } else { sfu };
    let mut roster: Vec<i64> = others.iter().copied().filter(|other| other != device).collect();
    roster.push(1);
    viewers.push(join(address, certificate_hash, *device, &roster).await);
  }
  wait_until_secured(&mut presenter).await;
  for viewer in &mut viewers {
    wait_until_secured(viewer).await;
  }

  let camera = BusyCamera::new(start);
  let pictures = camera.pictures();
  let shown = camera.shown();
  let encoded: Arc<Mutex<Vec<Encoded>>> = Arc::default();
  let config = VideoSendConfig {
    kind: VideoTrackKind::Camera,
    capture: Box::new(camera),
    layers: simulcast_layers(HD, FRAMERATE, CAMERA_BITRATE_BPS, 3),
    // OpenH264 on every platform: VideoToolbox keeps this picture far below its bitrate (720p at
    // about 140 kbit/s), so no limit here would limit it.
    encoders: counting(software_encoders(), start, Arc::clone(&encoded)),
    maybe_filter: None,
    maybe_idle_refresh: None,
  };
  presenter.start_video(config).await.unwrap();

  let is_done = Arc::new(AtomicBool::new(false));
  let heard: Arc<Mutex<Vec<(Duration, Heard)>>> = Arc::default();
  let presenter_task = tokio::spawn(listen_as_presenter(
    presenter,
    start,
    Arc::clone(&heard),
    Arc::clone(&is_done),
  ));
  let mut viewer_tasks = Vec::new();
  let mut decoded = Vec::new();
  for viewer in viewers {
    let sink: Arc<Mutex<Vec<Decoded>>> = Arc::default();
    decoded.push(Arc::clone(&sink));
    viewer_tasks.push(tokio::spawn(watch_as_viewer(
      viewer,
      start,
      Watching {
        sink,
        pictures: Arc::clone(&pictures),
        shown: Arc::clone(&shown),
      },
      Arc::clone(&is_done),
    )));
  }

  let mut timeline_steps = Vec::new();
  for (maybe_limit, duration) in steps {
    link.set_limit(*maybe_limit);
    timeline_steps.push((start.elapsed(), *maybe_limit));
    tokio::time::sleep(*duration).await;
  }
  let end = start.elapsed();
  is_done.store(true, Ordering::Relaxed);
  let presenter = presenter_task.await.unwrap();
  presenter.stop_video(VideoTrackKind::Camera);
  presenter.leave().await.unwrap();
  for task in viewer_tasks {
    task.await.unwrap().leave().await.unwrap();
  }
  let decoded = decoded.iter().map(|sink| sink.lock().unwrap().clone()).collect();
  let encoded = encoded.lock().unwrap().clone();
  let heard = heard.lock().unwrap().clone();
  let link = link.log.lock().unwrap().clone();
  Timeline {
    steps: timeline_steps,
    maybe_end: Some(end),
    decoded,
    encoded,
    heard,
    link,
  }
}

/// Records the presenter's layers and estimates until `is_done`.
async fn listen_as_presenter(
  mut presenter: NativeCall,
  start: Instant,
  heard: Arc<Mutex<Vec<(Duration, Heard)>>>,
  is_done: Arc<AtomicBool>,
) -> NativeCall {
  while !is_done.load(Ordering::Relaxed) {
    let maybe_heard = match tokio::time::timeout(Duration::from_millis(20), presenter.next_event()).await {
      Ok(Some(CallEngineEvent::LayersNeeded { layers, .. })) => Some(Heard::LayersNeeded(layers)),
      Ok(Some(CallEngineEvent::LayerBitrates { bitrates, .. })) => Some(Heard::LayerBitrates(bitrates)),
      Ok(Some(CallEngineEvent::UplinkEstimate { bps, .. })) => Some(Heard::UplinkEstimate(bps)),
      _ => None,
    };
    if let Some(event) = maybe_heard {
      heard.lock().unwrap().push((start.elapsed(), event));
    }
  }
  presenter
}

/// What a viewer needs to judge its pictures.
struct Watching {
  sink: Arc<Mutex<Vec<Decoded>>>,
  pictures: Arc<Vec<VideoFrame>>,
  shown: Arc<Mutex<BTreeMap<u64, usize>>>,
}

/// Watches the presenter's camera (top layer asked for, as the desktop's large tile) until
/// `is_done`.
async fn watch_as_viewer(
  mut viewer: NativeCall,
  start: Instant,
  watching: Watching,
  is_done: Arc<AtomicBool>,
) -> NativeCall {
  let Watching {
    sink,
    pictures,
    shown,
  } = watching;
  let mut watching = Some((sink, pictures, shown));
  while !is_done.load(Ordering::Relaxed) {
    if let Ok(Some(CallEngineEvent::VideoPublished(track))) =
      tokio::time::timeout(Duration::from_millis(20), viewer.next_event()).await
      && track.kind == VideoTrackKind::Camera
      && let Some((sink, pictures, shown)) = watching.take()
    {
      let mut count = 0usize;
      viewer.watch_video(
        track.stream_id,
        None,
        Box::new(move |frame| {
          let at = start.elapsed();
          count += 1;
          let resolution = frame.resolution();
          let maybe_psnr_db = count.is_multiple_of(5).then(|| {
            let index = shown_at(&shown.lock().unwrap(), frame.timestamp_us());
            // As a large tile shows it: a lower layer scaled up.
            luma_psnr(&pictures[index], &frame.scaled(HD).unwrap())
          });
          sink.lock().unwrap().push(Decoded {
            at,
            resolution,
            maybe_psnr_db,
          });
        }),
      );
    }
  }
  viewer
}

impl Timeline {
  /// When the last step ended.
  fn end(&self) -> Duration {
    self.maybe_end.unwrap_or_default()
  }

  /// Pictures `viewer` decoded from `from` to `to`.
  fn decoded_between(&self, viewer: usize, from: Duration, to: Duration) -> impl Iterator<Item = &Decoded> {
    self.decoded[viewer]
      .iter()
      .filter(move |picture| picture.at >= from && picture.at < to)
  }

  /// The share of `viewer`'s pictures from `from` to `to` that are 720p.
  fn share_of_hd(&self, viewer: usize, from: Duration, to: Duration) -> f64 {
    self.share_where(viewer, from, to, |picture| picture.resolution == HD)
  }

  /// The share of `viewer`'s pictures from `from` to `to` at least `width` wide.
  fn share_at_least(&self, viewer: usize, width: u16, from: Duration, to: Duration) -> f64 {
    self.share_where(viewer, from, to, |picture| picture.resolution.width >= width)
  }

  fn share_where(
    &self,
    viewer: usize,
    from: Duration,
    to: Duration,
    is_counted: impl Fn(&Decoded) -> bool,
  ) -> f64 {
    let (counted, all) = self
      .decoded_between(viewer, from, to)
      .fold((0usize, 0usize), |(counted, all), picture| {
        (counted + usize::from(is_counted(picture)), all + 1)
      });
    if all == 0 {
      return 0.0;
    }
    counted as f64 / all as f64
  }

  /// The share of the packets on the limited link from `from` to `to` that its queue dropped.
  fn dropped_share(&self, from: Duration, to: Duration) -> f64 {
    let (dropped, all) = self
      .link
      .iter()
      .filter(|(at, _, _)| *at >= from && *at < to)
      .fold((0usize, 0usize), |(dropped, all), (_, _, is_dropped)| {
        (dropped + usize::from(*is_dropped), all + 1)
      });
    if all == 0 {
      return 0.0;
    }
    dropped as f64 / all as f64
  }

  /// `viewer`'s picture never stood still for longer than [`MAX_STALL`] from its first picture to
  /// the end.
  fn assert_no_stall(&self, viewer: usize, summary: &str) {
    let decoded = &self.decoded[viewer];
    assert!(!decoded.is_empty(), "viewer {viewer} decoded nothing:\n{summary}");
    let longest = decoded
      .windows(2)
      .map(|pair| (pair[1].at.saturating_sub(pair[0].at), pair[1].at))
      .chain(
        decoded
          .last()
          .map(|last| (self.end().saturating_sub(last.at), self.end())),
      )
      .max_by_key(|(gap, _)| *gap)
      .unwrap_or_default();
    assert!(
      longest.0 <= MAX_STALL,
      "viewer {viewer}'s picture stood still for {:?} until {:?}:\n{summary}",
      longest.0,
      longest.1
    );
  }

  /// Most of `viewer`'s pictures in the last [`FINAL_STRETCH`] are 720p again.
  fn assert_back_to_hd(&self, viewer: usize, summary: &str) {
    let end = self.end();
    let hd = self.share_of_hd(viewer, end.saturating_sub(FINAL_STRETCH), end);
    assert!(
      hd > 0.66,
      "viewer {viewer} got {hd:.2} of its last pictures at 720p:\n{summary}"
    );
  }

  /// One line per second: the link's limit and what crossed it, each viewer's pictures, the
  /// presenter's output per layer, the layers it sent and its uplink estimate.
  fn summary(&self) -> String {
    let seconds = self
      .steps
      .iter()
      .map(|(at, _)| at.as_secs() + 10)
      .max()
      .unwrap_or(0)
      .max(
        self
          .encoded
          .iter()
          .map(|picture| picture.at.as_secs())
          .max()
          .unwrap_or(0),
      );
    let mut out = String::from(
      "  s  limit  link kbit/s (dropped) | viewers: pictures size PSNR, longest gap ms | encoded kbit/s per layer width (keyframes) | layers sent, uplink estimate kbit/s\n",
    );
    for second in 0..=seconds {
      let maybe_limit = self
        .steps
        .iter()
        .rev()
        .find(|(at, _)| at.as_secs() <= second)
        .and_then(|(_, maybe_limit)| *maybe_limit);
      let limit = maybe_limit.map_or("-".to_string(), |bps| format!("{}k", bps / 1000));
      let (sent, dropped) = self.link.iter().filter(|(at, _, _)| at.as_secs() == second).fold(
        (0, 0),
        |(sent, dropped), (_, bytes, is_dropped)| {
          if *is_dropped {
            (sent, dropped + 1)
          } else {
            (sent + bytes, dropped)
          }
        },
      );
      let viewers: Vec<String> = self
        .decoded
        .iter()
        .map(|decoded| viewer_second(decoded, second))
        .collect();
      let mut by_layer: BTreeMap<u16, (usize, usize)> = BTreeMap::new();
      for picture in self
        .encoded
        .iter()
        .filter(|picture| picture.at.as_secs() == second)
      {
        let entry = by_layer.entry(picture.width).or_default();
        entry.0 += picture.bytes;
        entry.1 += usize::from(picture.is_keyframe);
      }
      let encoded: Vec<String> = by_layer
        .iter()
        .map(|(width, (bytes, keyframes))| format!("{width}: {} ({keyframes})", bytes * 8 / 1000))
        .collect();
      let mut presenter = Vec::new();
      let mut maybe_range: Option<(u32, u32)> = None;
      let close = |presenter: &mut Vec<String>, maybe_range: &mut Option<(u32, u32)>| match maybe_range.take()
      {
        Some((low, high)) if low / 1000 == high / 1000 => presenter.push(format!("{}", low / 1000)),
        Some((low, high)) => presenter.push(format!("{}-{}", low / 1000, high / 1000)),
        None => {},
      };
      for (_, heard) in self.heard.iter().filter(|(at, _)| at.as_secs() == second) {
        match heard {
          Heard::LayersNeeded(layers) => {
            close(&mut presenter, &mut maybe_range);
            presenter.push(format!("{layers:?}"));
          },
          Heard::LayerBitrates(bitrates) => {
            close(&mut presenter, &mut maybe_range);
            let kbps: Vec<u32> = bitrates.iter().map(|(_, bps)| bps / 1000).collect();
            presenter.push(format!("kbit/s {kbps:?}"));
          },
          Heard::UplinkEstimate(bps) => {
            let (low, high) = maybe_range.unwrap_or((*bps, *bps));
            maybe_range = Some((low.min(*bps), high.max(*bps)));
          },
        }
      }
      close(&mut presenter, &mut maybe_range);
      let _ = writeln!(
        out,
        "{second:3}  {limit:>5}  {:>5} ({dropped:>3}) | {} | {:<40} | {}",
        sent * 8 / 1000,
        viewers.join(" | "),
        encoded.join(", "),
        presenter.join(" ")
      );
    }
    out
  }
}

/// One viewer's pictures in `second`: count, size(s), mean PSNR and the longest gap before a
/// picture.
fn viewer_second(decoded: &[Decoded], second: u64) -> String {
  let in_second: Vec<(usize, &Decoded)> = decoded
    .iter()
    .enumerate()
    .filter(|(_, picture)| picture.at.as_secs() == second)
    .collect();
  if in_second.is_empty() {
    return format!("{:<30}", "none");
  }
  let mut sizes: Vec<String> = Vec::new();
  for (_, picture) in &in_second {
    let size = format!("{}x{}", picture.resolution.width, picture.resolution.height);
    if !sizes.contains(&size) {
      sizes.push(size);
    }
  }
  let psnrs: Vec<f64> = in_second
    .iter()
    .filter_map(|(_, picture)| picture.maybe_psnr_db)
    .collect();
  let psnr = if psnrs.is_empty() {
    "-".to_string()
  } else {
    format!("{:.1}", psnrs.iter().sum::<f64>() / psnrs.len() as f64)
  };
  let longest_gap = in_second
    .iter()
    .filter(|(index, _)| *index > 0)
    .map(|(index, picture)| picture.at.saturating_sub(decoded[index - 1].at))
    .max()
    .unwrap_or_default();
  format!(
    "{:<30}",
    format!(
      "{:2} {} {psnr} {}",
      in_second.len(),
      sizes.join("+"),
      longest_gap.as_millis()
    )
  )
}

/// `encoders`, recording every picture's size per layer.
fn counting(encoders: EncoderFactory, start: Instant, log: Arc<Mutex<Vec<Encoded>>>) -> EncoderFactory {
  Arc::new(move |layer, content| {
    Ok(Box::new(CountingEncoder {
      inner: encoders(layer, content)?,
      width: layer.width,
      start,
      log: Arc::clone(&log),
    }) as Box<dyn VideoEncoder>)
  })
}

struct CountingEncoder {
  inner: Box<dyn VideoEncoder>,
  width: u16,
  start: Instant,
  log: Arc<Mutex<Vec<Encoded>>>,
}

impl VideoEncoder for CountingEncoder {
  fn codec(&self) -> VideoCodec {
    self.inner.codec()
  }

  fn name(&self) -> &'static str {
    self.inner.name()
  }

  fn encode(
    &mut self,
    frame: &VideoFrame,
    is_keyframe_forced: bool,
  ) -> Result<Option<EncodedPicture>, VideoCodecError> {
    let maybe_picture = self.inner.encode(frame, is_keyframe_forced)?;
    if let Some(picture) = &maybe_picture {
      self.log.lock().unwrap().push(Encoded {
        at: self.start.elapsed(),
        width: self.width,
        bytes: picture.data.len(),
        is_keyframe: picture.is_keyframe,
      });
    }
    Ok(maybe_picture)
  }
}

/// A camera that looks busy to an encoder: a lit background with texture, a head and a hand that
/// move, and sensor noise in every picture. The pictures are drawn once and repeat every second.
struct BusyCamera {
  pictures: Arc<Vec<VideoFrame>>,
  /// Which picture was delivered at which timestamp.
  shown: Arc<Mutex<BTreeMap<u64, usize>>>,
  start: Instant,
  maybe_running: Option<(std_mpsc::Sender<()>, JoinHandle<()>)>,
}

impl BusyCamera {
  fn new(start: Instant) -> Self {
    let pictures = (0..CAMERA_PICTURES).map(draw_camera_picture).collect();
    Self {
      pictures: Arc::new(pictures),
      shown: Arc::default(),
      start,
      maybe_running: None,
    }
  }

  fn pictures(&self) -> Arc<Vec<VideoFrame>> {
    Arc::clone(&self.pictures)
  }

  fn shown(&self) -> Arc<Mutex<BTreeMap<u64, usize>>> {
    Arc::clone(&self.shown)
  }
}

impl VideoCapture for BusyCamera {
  fn format(&self) -> CaptureFormat {
    CaptureFormat {
      resolution: HD,
      framerate: FRAMERATE,
    }
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    let (stop, stopped) = std_mpsc::channel::<()>();
    let (pictures, shown, start) = (Arc::clone(&self.pictures), Arc::clone(&self.shown), self.start);
    let thread = std::thread::spawn(move || {
      let period = Duration::from_secs(1) / u32::from(FRAMERATE);
      let mut next = Instant::now();
      for index in 0usize.. {
        let picture = index % pictures.len();
        let timestamp_us = u64::try_from(start.elapsed().as_micros()).unwrap();
        shown.lock().unwrap().insert(timestamp_us, picture);
        if !frames.send(pictures[picture].clone().with_timestamp_us(timestamp_us)) {
          return;
        }
        next += period;
        let wait = next.saturating_duration_since(Instant::now());
        if !matches!(
          stopped.recv_timeout(wait),
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

/// Picture `index` of the busy camera (I420).
fn draw_camera_picture(index: usize) -> VideoFrame {
  let (width, height) = (usize::from(HD.width), usize::from(HD.height));
  let phase = index as f64 / CAMERA_PICTURES as f64 * std::f64::consts::TAU;
  // The head sways, the hand waves faster.
  let head = (640.0 + 40.0 * phase.sin(), 330.0 + 8.0 * (2.0 * phase).sin());
  let hand = (
    900.0 + 90.0 * (2.0 * phase).cos(),
    520.0 + 60.0 * (2.0 * phase).sin(),
  );
  let mut random = (index as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
  let mut noise = move || {
    random ^= random << 13;
    random ^= random >> 7;
    random ^= random << 17;
    (random % 9) as i32 - 4
  };
  let mut data = Vec::with_capacity(width * height * 3 / 2);
  for y in 0..height {
    for x in 0..width {
      // A wall lit from the left, with a bookshelf's texture on the right.
      let mut luma = 180 - (x * 60 / width) as i32 - (y * 30 / height) as i32;
      if x > 1000 && (y / 40 + x / 24) % 3 == 0 {
        luma -= 50 + ((x * 7 + y * 3) % 23) as i32;
      }
      let (dx, dy) = (x as f64 - head.0, y as f64 - head.1);
      if (dx / 150.0).powi(2) + (dy / 200.0).powi(2) < 1.0 {
        // Skin, with hair on top and the eyes' shadows.
        luma = 150 + (dx / 20.0) as i32;
        if dy < -120.0 {
          luma = 50 + ((x + y) % 7) as i32;
        } else if (-40.0..-20.0).contains(&dy) && (40.0..70.0).contains(&dx.abs()) {
          luma = 60;
        }
      }
      let (hx, hy) = (x as f64 - hand.0, y as f64 - hand.1);
      if hx * hx + hy * hy < 55.0 * 55.0 {
        luma = 140 + (hy / 8.0) as i32;
      }
      data.push((luma + noise()).clamp(16, 235) as u8);
    }
  }
  for plane in [0u8, 1] {
    for y in 0..height / 2 {
      for x in 0..width / 2 {
        let (dx, dy) = ((x * 2) as f64 - head.0, (y * 2) as f64 - head.1);
        let is_skin = (dx / 150.0).powi(2) + (dy / 200.0).powi(2) < 1.0 && dy >= -120.0;
        let value = match (plane, is_skin) {
          (0, true) => 110,
          (_, true) => 150,
          (0, false) => 128 + (x * 10 / (width / 2)) as i32,
          (_, false) => 124,
        };
        data.push((value + noise() / 2) as u8);
      }
    }
  }
  VideoFrame::new(HD, 0, data).unwrap()
}

/// The picture shown at `timestamp_us`: the last one delivered up to then (RTP's 90 kHz clock
/// rounds).
fn shown_at(shown: &BTreeMap<u64, usize>, timestamp_us: u64) -> usize {
  shown
    .range(..=timestamp_us + 100)
    .next_back()
    .map(|(_, picture)| *picture)
    .unwrap_or_else(|| panic!("nothing shown up to {timestamp_us} µs"))
}

/// Luma PSNR of two pictures of the same size.
fn luma_psnr(source: &VideoFrame, decoded: &VideoFrame) -> f64 {
  let (a, b) = (source.planes().0, decoded.planes().0);
  let squared_error: u64 = a
    .iter()
    .zip(b)
    .map(|(x, y)| u64::from(x.abs_diff(*y)).pow(2))
    .sum();
  let mse = squared_error as f64 / a.len() as f64;
  10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10()
}

/// Relays one client's UDP to the SFU and back; each direction can be limited like a bottleneck.
struct Relay {
  address: SocketAddr,
  to_sfu: Shaper,
  to_client: Shaper,
}

/// Packets with the time they leave the bottleneck.
type Departures = mpsc::UnboundedReceiver<(tokio::time::Instant, Vec<u8>)>;

/// One direction of a [`Relay`]: packets leave at the limit's rate after waiting in a queue of
/// at most [`MAX_QUEUE_DELAY`]; a packet that would wait longer is dropped.
#[derive(Clone)]
struct Shaper {
  /// Bit/s; 0 for no limit.
  limit_bps: Arc<AtomicU64>,
  start: Instant,
  log: Arc<Mutex<Vec<(Duration, usize, bool)>>>,
}

impl Shaper {
  fn new(start: Instant) -> Self {
    Self {
      limit_bps: Arc::default(),
      start,
      log: Arc::default(),
    }
  }

  fn set_limit(&self, maybe_limit_bps: Option<u64>) {
    self
      .limit_bps
      .store(maybe_limit_bps.unwrap_or(0), Ordering::Relaxed);
  }

  /// The queue: packets go into the sender, and come out of the receiver with the time they
  /// leave (dropped ones don't).
  fn spawn_queue(&self) -> (mpsc::UnboundedSender<Vec<u8>>, Departures) {
    let (packets, mut incoming) = mpsc::unbounded_channel::<Vec<u8>>();
    let (departures, queue) = mpsc::unbounded_channel();
    let shaper = self.clone();
    tokio::spawn(async move {
      let mut next_free = tokio::time::Instant::now();
      while let Some(packet) = incoming.recv().await {
        let now = tokio::time::Instant::now();
        let at = shaper.start.elapsed();
        let limit_bps = shaper.limit_bps.load(Ordering::Relaxed);
        let departure = next_free.max(now);
        if limit_bps > 0 && departure.saturating_duration_since(now) > MAX_QUEUE_DELAY {
          shaper.log.lock().unwrap().push((at, packet.len(), true));
          continue;
        }
        if limit_bps > 0 {
          next_free = departure + Duration::from_secs_f64(packet.len() as f64 * 8.0 / limit_bps as f64);
        } else {
          next_free = departure;
        }
        shaper.log.lock().unwrap().push((at, packet.len(), false));
        if departures.send((departure, packet)).is_err() {
          return;
        }
      }
    });
    (packets, queue)
  }
}

impl Relay {
  async fn start(sfu: SocketAddr, start: Instant) -> Self {
    let client_side = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    let sfu_side = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    sfu_side.connect(sfu).await.unwrap();
    let address = client_side.local_addr().unwrap();
    let (to_sfu, to_client) = (Shaper::new(start), Shaper::new(start));
    let client: Arc<Mutex<Option<SocketAddr>>> = Arc::default();

    let (into_sfu_queue, mut sfu_queue) = to_sfu.spawn_queue();
    let (from_client, known_client) = (Arc::clone(&client_side), Arc::clone(&client));
    tokio::spawn(async move {
      let mut buffer = vec![0; 65_536];
      while let Ok((len, from)) = from_client.recv_from(&mut buffer).await {
        *known_client.lock().unwrap() = Some(from);
        if into_sfu_queue.send(buffer[..len].to_vec()).is_err() {
          return;
        }
      }
    });
    let towards_sfu = Arc::clone(&sfu_side);
    tokio::spawn(async move {
      while let Some((departure, packet)) = sfu_queue.recv().await {
        tokio::time::sleep_until(departure).await;
        let _ = towards_sfu.send(&packet).await;
      }
    });

    let (into_client_queue, mut client_queue) = to_client.spawn_queue();
    tokio::spawn(async move {
      let mut buffer = vec![0; 65_536];
      while let Ok(len) = sfu_side.recv(&mut buffer).await {
        if into_client_queue.send(buffer[..len].to_vec()).is_err() {
          return;
        }
      }
    });
    tokio::spawn(async move {
      while let Some((departure, packet)) = client_queue.recv().await {
        tokio::time::sleep_until(departure).await;
        let maybe_client = *client.lock().unwrap();
        if let Some(client) = maybe_client {
          let _ = client_side.send_to(&packet, client).await;
        }
      }
    });
    Self {
      address,
      to_sfu,
      to_client,
    }
  }
}

async fn wait_until_secured(call: &mut NativeCall) {
  let deadline = tokio::time::Instant::now() + STEP_DEADLINE;
  loop {
    assert!(
      tokio::time::Instant::now() < deadline,
      "the call was never secured"
    );
    if let Ok(Some(CallEngineEvent::Secured { .. })) =
      tokio::time::timeout(Duration::from_millis(50), call.next_event()).await
    {
      return;
    }
  }
}

async fn join(sfu: SocketAddr, certificate_hash: [u8; 32], device: i64, others: &[i64]) -> NativeCall {
  let engine = CallEngineConfig {
    user_id: id(device),
    device_id: id(device),
    capabilities: MediaCapabilities {
      audio_codecs: vec![AudioCodec::Opus],
      video_encoders: vec![VideoCodec::H264],
      video_decoders: vec![VideoCodec::H264],
      max_simulcast_layers: 3,
      max_frame_fragments: teamcraft_proto::MAX_VIDEO_FRAME_FRAGMENTS,
    },
    bwe: BweConfig::default(),
  };
  let sfu_url = format!("https://{sfu}{CALL_PATH}");
  let mut config = NativeCallConfig::new(sfu_url, format!("dev:14:{device}:{device}"), engine);
  config.maybe_certificate_hash = Some(certificate_hash);
  config.maybe_trusted_roster = Some(others.iter().map(|other| id::<DeviceId>(*other)).collect());
  config.source = AudioSource::Tone {
    frequency_hz: 440.0,
    amplitude: 0.3,
  };
  NativeCall::join(config).await.unwrap()
}

async fn start_sfu() -> (u16, [u8; 32]) {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
  let port = sfu.local_addr().unwrap().port();
  let certificate_hash = *sfu.certificate_hash().as_ref();
  tokio::spawn(sfu.run());
  (port, certificate_hash)
}

fn id<T: std::str::FromStr>(raw: i64) -> T
where
  T::Err: std::fmt::Debug,
{
  raw.to_string().parse().unwrap()
}
