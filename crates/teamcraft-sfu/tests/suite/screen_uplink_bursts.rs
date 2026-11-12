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
//! The asserting test judges the link, not the machine: from the slide's first picture out of
//! the encoder to the viewer showing it. OpenH264 in a debug build on a CI runner encoded the
//! Retina-size screen at a picture every few seconds, and the slides waited for the encoder, not
//! the link (5 to 9 s, nothing dropped). So it also shares a smaller window ([`WINDOW`],
//! 1108×720), through an uplink that is a fixed multiple of the share's layers
//! ([`LIMIT_SHARE`]). The measurement shares the Retina-size screen.
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
use teamcraft_media::video::{ScreenProfile, VideoFrame, VideoResolution, screen_layers};
use teamcraft_media::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture};
use teamcraft_media::video_codec::{EncodedPicture, VideoCodecError, VideoEncoder};
use teamcraft_media::video_pipeline::{EncoderFactory, VideoSendConfig, preferred_encoders};
use teamcraft_proto::{ScreenContentMode, ScreenShareResolution, VideoCodec, VideoLayerInfo, VideoTrackKind};

use crate::common::relay::Relay;
use crate::screen_sharpness::{SHARED, fill, join, pump_for, shown_at, start_sfu, text, wait_until_secured};

/// How long a slide stays.
const SLIDE_INTERVAL: Duration = Duration::from_secs(2);
/// Distinct slides, shown in turn.
const SLIDES: usize = 3;
/// The share's frame rate (detail mode's).
const FRAMERATE: u8 = 30;
/// How long the call carries only sound before the share: the uplink estimate measures the link.
const SOUND_ONLY: Duration = Duration::from_secs(4);
/// How long the share is watched.
const SHARE: Duration = Duration::from_secs(24);
/// The first slides go out while the estimate still grows and the viewer gets its layers: they
/// aren't judged.
const SETTLE: Duration = Duration::from_secs(8);
/// The shared window of the asserting test: half the Retina-size screen, small enough for OpenH264
/// in a debug build on a CI runner (two layers: 1108×720 and 554×360).
const WINDOW: VideoResolution = VideoResolution {
  width: SHARED.width / 2,
  height: SHARED.height / 2,
};
/// The uplink of the asserting test, as a multiple of what the share's layers ask for: room for
/// both layers and the sound (the uplink allocation wants 1.27 times the layers for all of
/// them), not for a slide of both layers at once within the bottleneck's queue. A 1440p share at
/// this multiple is the 12 Mbit/s row of the measurement.
const LIMIT_SHARE: f64 = 1.45;
/// The longest from a slide's first picture out of the encoder to the viewer showing it, once the
/// share settled: under 0.25 s on a laptop in a debug build (0.3 s from the slide's capture);
/// room for a slow CI runner's decoding and encryption.
const MAX_LINK_DELAY: Duration = Duration::from_secs(2);
/// Slides that have to be judged: some, also where the encoder can't keep up.
const MIN_JUDGED_SLIDES: usize = 3;

/// #1005: slides of text reach the viewer through an uplink with room for the share but not for
/// its bursts at once, each within seconds, and the bottleneck drops next to nothing. The desktop's
/// share steps its rate with the uplink in place (#1781): no keyframe beyond those asked for and
/// those of new layers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slides_reach_the_viewer_through_a_limited_uplink() {
  let layers_bps: u64 = share_layers(WINDOW)
    .iter()
    .map(|layer| u64::from(layer.max_bitrate_bps))
    .sum();
  let run = run(WINDOW, Some((layers_bps as f64 * LIMIT_SHARE) as u64), None).await;
  let summary = run.summary();
  println!("{summary}");
  // Slides the encoder gave a picture of: each has to reach the viewer, soon after.
  let arrivals: Vec<Arrival> = run
    .link_delays()
    .into_iter()
    .filter(|arrival| *arrival != Arrival::NotEncoded)
    .collect();
  assert!(
    arrivals.len() >= MIN_JUDGED_SLIDES,
    "only {} slides were encoded:\n{summary}",
    arrivals.len()
  );
  assert!(
    !arrivals.contains(&Arrival::Lost),
    "an encoded slide never reached the viewer:\n{summary}"
  );
  let worst = arrivals
    .iter()
    .filter_map(|arrival| match arrival {
      Arrival::After(delay) => Some(*delay),
      _ => None,
    })
    .max()
    .unwrap();
  assert!(
    worst <= MAX_LINK_DELAY,
    "a slide took {worst:?} from the encoder to the viewer:\n{summary}"
  );
  let dropped = run.dropped_share();
  assert!(
    dropped < 0.05,
    "{:.1} % of the uplink's bytes dropped:\n{summary}",
    dropped * 100.0
  );
  let keyframes = run.keyframes();
  assert_eq!(
    keyframes.rebuilt_for_rate, 0,
    "a rate change rebuilt an encoder:\n{summary}"
  );
  assert_eq!(
    keyframes.built,
    keyframes.republished * run.layers.len(),
    "encoders built for anything but new layers:\n{summary}"
  );
  assert!(
    keyframes.forced <= keyframes.asked,
    "more keyframes forced than asked for:\n{summary}"
  );
}

/// #1781: an uplink that dips to half of what the share's layers ask for and comes back makes the
/// share step its rate (the top layer at a share of its bitrate, at a lower frame rate). The
/// encoders take every step in place: none is rebuilt, and no keyframe comes beyond those asked
/// for (viewers' requests, layers needed again). Before, each step rebuilt the encoder, and half
/// the top layer's pictures were keyframes (12 of 24 on CI).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dipping_uplink_steps_the_share_in_place() {
  let layers_bps: u64 = share_layers(WINDOW)
    .iter()
    .map(|layer| u64::from(layer.max_bitrate_bps))
    .sum();
  let dip = Dip {
    after: SETTLE,
    lasts: Duration::from_secs(8),
    limit_bps: layers_bps / 2,
  };
  let run = run(WINDOW, Some((layers_bps as f64 * LIMIT_SHARE) as u64), Some(dip)).await;
  let summary = run.summary();
  println!("{summary}");
  let keyframes = run.keyframes();
  assert!(keyframes.in_place > 0, "the dip changed no rate:\n{summary}");
  assert_eq!(
    keyframes.rebuilt_for_rate, 0,
    "a rate change rebuilt an encoder:\n{summary}"
  );
  assert_eq!(
    keyframes.built,
    keyframes.republished * run.layers.len(),
    "encoders built for anything but new layers:\n{summary}"
  );
  assert!(
    keyframes.forced <= keyframes.asked,
    "more keyframes forced than asked for:\n{summary}"
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
    let run = run(SHARED, maybe_limit_bps, None).await;
    println!("== uplink {maybe_limit_bps:?} bit/s\n{}", run.summary());
  }
}

/// A stretch of a run with a narrower uplink.
struct Dip {
  /// Since the share's start.
  after: Duration,
  lasts: Duration,
  limit_bps: u64,
}

/// What a run recorded, timed from the share's start.
struct Run {
  /// The shared screen's size.
  size: VideoResolution,
  layers: Vec<VideoLayerInfo>,
  /// When each slide went up, and its capture timestamp.
  changes: Vec<(Duration, u64)>,
  /// The viewer's pictures: when, the capture timestamp, the size.
  received: Vec<(Duration, u64, VideoResolution)>,
  shown: BTreeMap<u64, usize>,
  encoded: Vec<Encoded>,
  /// When an encoder was built (the share's start, new layers, or a rate an encoder couldn't take
  /// in place).
  built: Vec<Duration>,
  /// When an encoder was given a new rate, and whether it took it in place.
  rate_changes: Vec<(Duration, bool)>,
  /// The uplink's packets: when, bytes, whether dropped.
  link: Vec<(Duration, usize, bool)>,
  /// The presenter's uplink estimates: when, bit/s.
  uplink_estimates: Vec<(Duration, u32)>,
  /// The layers the presenter was told to encode (the SFU's need within the uplink): when, which.
  layers_needed: Vec<(Duration, Vec<u8>)>,
  /// When a viewer (through the SFU) asked the presenter for a keyframe.
  keyframe_requests: Vec<Duration>,
  /// When the share went out anew with other layers (a size step of the share policy).
  republished: Vec<Duration>,
  started_at: Duration,
}

/// The desktop's layers for a detail share of a screen of `size`, as it starts.
fn share_layers(size: VideoResolution) -> Vec<VideoLayerInfo> {
  let profile = profile();
  screen_layers(size, FRAMERATE, profile.mode, &profile.size)
}

/// The desktop's share: detail at the standard size.
fn profile() -> ScreenProfile {
  ScreenProfile {
    mode: ScreenContentMode::Detail,
    size: ScreenShareResolution::Standard,
  }
}

/// Shares slides at `size` through an uplink limited to `maybe_limit_bps`.
async fn run(size: VideoResolution, maybe_limit_bps: Option<u64>, maybe_dip: Option<Dip>) -> Run {
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

  let slides = SlideShow::new(start, size);
  let (shown, changes) = (Arc::clone(&slides.shown), Arc::clone(&slides.changes));
  let log = EncoderLog::default();
  let layers = share_layers(size);
  let started_at = start.elapsed();
  let config = VideoSendConfig {
    maybe_layer_plan: None,
    kind: VideoTrackKind::Screen,
    layers: layers.clone(),
    capture: Box::new(slides),
    encoders: counting(preferred_encoders(), start, log.clone()),
    maybe_filter: None,
    maybe_picture_tap: None,
    maybe_idle_refresh: VideoSendConfig::default_idle_refresh(&VideoTrackKind::Screen),
    // The share steps along the share policy like the browser's (#1777), its rates in place
    // (#1781).
    maybe_screen_profile: Some(profile()),
  };
  presenter.start_video(config).await.unwrap();

  let received: Arc<Mutex<Vec<(Duration, u64, VideoResolution)>>> = Arc::default();
  let (mut uplink_estimates, mut layers_needed) = (Vec::new(), Vec::new());
  let (mut keyframe_requests, mut published) = (Vec::new(), Vec::new());
  let share_start = tokio::time::Instant::now();
  let end = share_start + SHARE;
  let mut is_dipped = false;
  while tokio::time::Instant::now() < end {
    if let Some(dip) = &maybe_dip {
      let since = share_start.elapsed();
      let should_dip = since >= dip.after && since < dip.after + dip.lasts;
      if should_dip != is_dipped {
        is_dipped = should_dip;
        relay.to_sfu.set_limit(if should_dip {
          Some(dip.limit_bps)
        } else {
          maybe_limit_bps
        });
      }
    }
    match tokio::time::timeout(Duration::from_millis(5), presenter.next_event()).await {
      Ok(Some(CallEngineEvent::LayersNeeded { layers, .. })) => layers_needed.push((start.elapsed(), layers)),
      Ok(Some(CallEngineEvent::UplinkEstimate { bps, .. })) => uplink_estimates.push((start.elapsed(), bps)),
      Ok(Some(CallEngineEvent::KeyframeRequested { .. })) => keyframe_requests.push(start.elapsed()),
      Ok(Some(CallEngineEvent::VideoPublished(track))) if track.kind == VideoTrackKind::Screen => {
        published.push(start.elapsed());
      },
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
  let encoded = log.pictures.lock().unwrap().clone();
  let built = log.built.lock().unwrap().clone();
  let rate_changes = log.rate_changes.lock().unwrap().clone();
  let link = relay.to_sfu.log.lock().unwrap().clone();
  Run {
    size,
    layers,
    changes,
    received,
    shown,
    encoded,
    built,
    rate_changes,
    link,
    uplink_estimates,
    layers_needed,
    keyframe_requests,
    // The first publication is the share's start.
    republished: published.into_iter().skip(1).collect(),
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
    self.delays(|resolution| resolution == self.size)
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

  /// Per slide after [`SETTLE`]: how it went from its first picture out of the encoder (any
  /// layer) to the viewer.
  fn link_delays(&self) -> Vec<Arrival> {
    let settled = self.started_at + SETTLE;
    let in_window =
      |picture_us: u64, from_us: u64, to_us: u64| picture_us + 100 >= from_us && picture_us + 100 < to_us;
    let mut delays = Vec::new();
    for pair in self.changes.windows(2) {
      let [(changed_at, timestamp_us), (_, next_us)] = pair else {
        continue;
      };
      if *changed_at < settled {
        continue;
      }
      let maybe_encoded_at = self
        .encoded
        .iter()
        .find(|picture| in_window(picture.timestamp_us, *timestamp_us, *next_us))
        .map(|picture| picture.at);
      let Some(encoded_at) = maybe_encoded_at else {
        delays.push(Arrival::NotEncoded);
        continue;
      };
      let maybe_shown_at = self
        .received
        .iter()
        .find(|(_, picture_us, _)| in_window(*picture_us, *timestamp_us, *next_us))
        .map(|(at, ..)| *at);
      delays.push(maybe_shown_at.map_or(Arrival::Lost, |shown_at| {
        Arrival::After(shown_at.saturating_sub(encoded_at))
      }));
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

  /// After [`SETTLE`], per cause: what the share sent and what explains it. #1781: a rate change
  /// rebuilt the encoder, which started with a keyframe (9 in 17 pictures through a dipping
  /// uplink); in place it sends none.
  fn keyframes(&self) -> Keyframes {
    let settled = self.started_at + SETTLE;
    let after_settle = |at: &Duration| *at >= settled;
    let pictures: Vec<&Encoded> = self
      .encoded
      .iter()
      .filter(|picture| after_settle(&picture.at))
      .collect();
    let count = |cause: KeyframeCause| {
      pictures
        .iter()
        .filter(|picture| picture.maybe_keyframe == Some(cause))
        .count()
    };
    let mut came_back = 0;
    for pair in self.layers_needed.windows(2) {
      let [(_, before), (at, after)] = pair else {
        continue;
      };
      if after_settle(at) {
        came_back += after.iter().filter(|layer| !before.contains(layer)).count();
      }
    }
    Keyframes {
      new_encoder: count(KeyframeCause::NewEncoder),
      forced: count(KeyframeCause::Forced),
      own: count(KeyframeCause::Encoders),
      built: self.built.iter().filter(|at| after_settle(at)).count(),
      republished: self.republished.iter().filter(|at| after_settle(at)).count(),
      asked: self
        .keyframe_requests
        .iter()
        .filter(|at| after_settle(at))
        .count()
        + came_back,
      in_place: self
        .rate_changes
        .iter()
        .filter(|(at, is_in_place)| after_settle(at) && *is_in_place)
        .count(),
      rebuilt_for_rate: self
        .rate_changes
        .iter()
        .filter(|(at, is_in_place)| after_settle(at) && !is_in_place)
        .count(),
    }
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
        "encoder to viewer (ms):      {}",
        self
          .link_delays()
          .iter()
          .map(|arrival| match arrival {
            Arrival::NotEncoded => "not encoded".to_owned(),
            Arrival::Lost => "-".to_owned(),
            Arrival::After(delay) => delay.as_millis().to_string(),
          })
          .collect::<Vec<_>>()
          .join(" ")
      ),
      format!(
        "uplink: {:.1} % of the bytes dropped after the first {} s; estimate {}–{} kbit/s after it; layers needed {:?}",
        self.dropped_share() * 100.0,
        SETTLE.as_secs(),
        self.estimate_range().0 / 1000,
        self.estimate_range().1 / 1000,
        self.layers_needed.last().map(|(_, layers)| layers),
      ),
      format!("after the first {} s: {:?}", SETTLE.as_secs(), self.keyframes()),
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

/// How a slide went from the encoder to the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrival {
  /// The encoder gave no picture of it (a machine too slow for every slide).
  NotEncoded,
  /// The viewer never showed it.
  Lost,
  /// The viewer showed it this long after its first picture.
  After(Duration),
}

/// A picture out of an encoder.
#[derive(Debug, Clone, Copy)]
struct Encoded {
  at: Duration,
  width: u16,
  bytes: usize,
  is_keyframe: bool,
  /// Why it is a keyframe, if it is one.
  maybe_keyframe: Option<KeyframeCause>,
  /// The capture timestamp of its frame.
  timestamp_us: u64,
}

/// Why a picture is a keyframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyframeCause {
  /// The first picture of an encoder just built.
  NewEncoder,
  /// The sender asked for it: a viewer's request, a layer needed again.
  Forced,
  /// The encoder chose it (a scene change, its keyframe interval).
  Encoders,
}

/// The keyframes after [`SETTLE`] by cause, and what explains them.
#[derive(Debug)]
// Some counts are only read through `Debug`, in the summary.
#[allow(dead_code)]
struct Keyframes {
  new_encoder: usize,
  forced: usize,
  own: usize,
  /// Encoders built, and times the share went out with new layers.
  built: usize,
  republished: usize,
  /// Keyframes asked for: viewers' requests and layers needed again.
  asked: usize,
  /// Rate changes the encoders took in place, and ones that rebuilt them.
  in_place: usize,
  rebuilt_for_rate: usize,
}

/// What the encoders did, shared by them.
#[derive(Default, Clone)]
struct EncoderLog {
  pictures: Arc<Mutex<Vec<Encoded>>>,
  built: Arc<Mutex<Vec<Duration>>>,
  rate_changes: Arc<Mutex<Vec<(Duration, bool)>>>,
}

/// `encoders`, recording every picture, every encoder built and every rate change.
fn counting(encoders: EncoderFactory, start: Instant, log: EncoderLog) -> EncoderFactory {
  Arc::new(move |layer, content| {
    log.built.lock().unwrap().push(start.elapsed());
    Ok(Box::new(CountingEncoder {
      inner: encoders(layer, content)?,
      width: layer.width,
      start,
      has_encoded: false,
      log: log.clone(),
    }) as Box<dyn VideoEncoder>)
  })
}

struct CountingEncoder {
  inner: Box<dyn VideoEncoder>,
  width: u16,
  start: Instant,
  has_encoded: bool,
  log: EncoderLog,
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
      let maybe_keyframe = picture.is_keyframe.then_some(if !self.has_encoded {
        KeyframeCause::NewEncoder
      } else if is_keyframe_forced {
        KeyframeCause::Forced
      } else {
        KeyframeCause::Encoders
      });
      self.has_encoded = true;
      self.log.pictures.lock().unwrap().push(Encoded {
        at: self.start.elapsed(),
        width: self.width,
        bytes: picture.data.len(),
        is_keyframe: picture.is_keyframe,
        maybe_keyframe,
        timestamp_us: frame.timestamp_us(),
      });
    }
    Ok(maybe_picture)
  }

  /// Passed on: the wrapper must not turn an in-place change into a rebuild (the trait's default
  /// answers "not in place").
  fn set_rate(&mut self, bitrate_bps: u32, framerate: u8) -> Result<bool, VideoCodecError> {
    let is_in_place = self.inner.set_rate(bitrate_bps, framerate)?;
    self
      .log
      .rate_changes
      .lock()
      .unwrap()
      .push((self.start.elapsed(), is_in_place));
    Ok(is_in_place)
  }
}

/// Slides of text, each a full change, delivered when they change (nothing between, as
/// ScreenCaptureKit does with a still screen).
struct SlideShow {
  /// The slides, at the shared size.
  sources: Arc<Vec<VideoFrame>>,
  size: VideoResolution,
  start: Instant,
  /// Which slide went up at which capture timestamp.
  shown: Arc<Mutex<BTreeMap<u64, usize>>>,
  /// When each slide went up (since `start`), and its capture timestamp.
  changes: Arc<Mutex<Vec<(Duration, u64)>>>,
  maybe_running: Option<(std_mpsc::Sender<()>, JoinHandle<()>)>,
}

impl SlideShow {
  /// Slides drawn at the Retina size and shrunk to `size` where it is another.
  fn new(start: Instant, size: VideoResolution) -> Self {
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
        let frame = picture.to_i420(0).unwrap();
        if size == SHARED {
          frame
        } else {
          frame.scaled(size).unwrap()
        }
      })
      .collect();
    Self {
      sources: Arc::new(sources),
      size,
      start,
      shown: Arc::default(),
      changes: Arc::default(),
      maybe_running: None,
    }
  }
}

impl VideoCapture for SlideShow {
  fn format(&self) -> CaptureFormat {
    CaptureFormat {
      resolution: self.size,
      framerate: FRAMERATE,
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
