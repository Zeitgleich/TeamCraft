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
use teamcraft_media::call_native::NativeCall;
use teamcraft_media::capture_convert::{PixelFormat, RawPicture};
use teamcraft_media::video::{ScreenProfile, VideoFrame, VideoResolution, screen_layers};
use teamcraft_media::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture};
use teamcraft_media::video_codec::{EncodedPicture, VideoCodecError, VideoEncoder};
use teamcraft_media::video_pipeline::{EncoderFactory, VideoSendConfig, preferred_encoders};
use teamcraft_proto::{ScreenContentMode, ScreenShareResolution, VideoCodec, VideoLayerInfo, VideoTrackKind};
use teamcraft_sfu::config::SfuConfig;

use crate::common::relay::{Impairment, Relay};
use crate::screen_sharpness::{SHARED, fill, join_with, shown_at, start_sfu_with, text, wait_until_secured};

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
/// A presenter's uplink on hotel or train Wi-Fi (#2189): 1 to 3 % of the video's packets lost, in
/// bursts, 20 to 60 ms one way, both fluctuating (`common::relay::Impairment`). The voice and the
/// transport's acknowledgements (packets under 300 bytes) are spared: lost as well, they make the
/// presenter hold its video for its voice (#1871) for the rest of the share (#2191), and the
/// measurement is about the video's repair.
const LOSSY_UPLINK: Impairment = Impairment {
  seed: 2189,
  loss_percent: (1.0, 3.0),
  delay: (Duration::from_millis(20), Duration::from_millis(60)),
  spared_below_bytes: 300,
};
/// Slides shown for one and for two seconds, in turn.
const SHORT_AND_LONG_SLIDES: &[Duration] = &[Duration::from_secs(1), Duration::from_secs(2)];
/// Viewers of the lossy uplink's share.
const LOSSY_VIEWERS: usize = 2;

/// #1005: slides of text reach the viewer through an uplink with room for the share but not for
/// its bursts at once, each within seconds, and the bottleneck drops next to nothing. The desktop's
/// share steps its rate with the uplink in place (#1781): no keyframe beyond those asked for and
/// those of new layers. Every encoded slide is judged, also one whose packets the bottleneck
/// dropped (#2175): the SFU asks the presenter for them at once (#2189).
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
    !arrivals.contains(&Arrival::Lost) && !arrivals.contains(&Arrival::Held),
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

/// Prints how slides reach the viewers through the lossy uplink with and without the SFU's repair
/// of the presenter's uplink (#2189), and with the voice lost as well. The real client's estimator
/// and voice guard make these runs vary (#2191: the video held for the voice, #2192: the estimate
/// cut to a still screen's rate by one burst); the asserting test of the repair runs on a
/// simulated clock (`uplink_loss`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement: prints a summary with and without the repair, takes about two minutes"]
async fn measure_slides_through_a_lossy_uplink() {
  for is_uplink_repaired in [true, false] {
    let run = run_scenario(lossy_scenario(is_uplink_repaired)).await;
    println!("== uplink repaired: {is_uplink_repaired}\n{}", run.summary());
  }
  let voice_lost_too = Scenario {
    maybe_impairment: Some(Impairment {
      spared_below_bytes: 0,
      ..LOSSY_UPLINK
    }),
    ..lossy_scenario(true)
  };
  let run = run_scenario(voice_lost_too).await;
  println!("== uplink repaired, the voice lost too\n{}", run.summary());
}

/// The share of [`measure_slides_through_a_lossy_uplink`].
fn lossy_scenario(is_uplink_repaired: bool) -> Scenario {
  Scenario {
    maybe_impairment: Some(LOSSY_UPLINK),
    slide_intervals: SHORT_AND_LONG_SLIDES,
    viewers: LOSSY_VIEWERS,
    is_uplink_repaired,
    ..Scenario::new(WINDOW)
  }
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

/// #2417: every viewer saw shared screens flicker. Each size or codec step of the share policy
/// (#1777) published the share anew: viewers saw it end and start, the share stage was built
/// again, the tile stood empty until the new track's keyframe, and every viewer's downlink and
/// the presenter's uplink were measured afresh, which invited the next step. Through an uplink
/// that swings to a quarter of the share every few seconds (a laptop on a train), the share now
/// goes on as one stream with other layers: no viewer sees it end, and each keeps getting
/// pictures. On main this was 10 ends and starts per viewer in 16 s (release build, 5 steps).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_swinging_uplink_keeps_the_share_one_stream_for_its_viewers() {
  let run = run_scenario(swinging_scenario(SHARED, 4)).await;
  let summary = run.summary();
  println!("{summary}");
  for (viewer, continuity) in run.continuity().iter().enumerate() {
    println!("viewer {viewer}: {continuity:?}");
    assert_eq!(
      continuity.restarts, 0,
      "viewer {viewer} saw the share end and start anew: {continuity:?}\n{summary}"
    );
    assert!(
      continuity.longest_gap < SHARE - SETTLE,
      "viewer {viewer} got no picture after the first {} s: {continuity:?}\n{summary}",
      SETTLE.as_secs()
    );
  }
}

/// #2417: an uplink that swings between room for the share and a half or a quarter of it, as on a
/// train or shared Wi-Fi, makes the Retina-size share step its size down and back up. Prints per
/// run and viewer whether its share stayed one track (a track that ends and starts anew rebuilds
/// the share stage: the flicker everyone saw) and how long its picture stood still, with the
/// presenter's holds of its video for its voice, which stand the picture still too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement: prints the viewers' continuity per uplink, takes about three minutes"]
async fn measure_share_continuity_on_a_swinging_uplink() {
  for divisor in [2, 4] {
    for round in 0..3 {
      let run = run_scenario(swinging_scenario(SHARED, divisor)).await;
      let keyframes = run.keyframes();
      for (viewer, continuity) in run.continuity().iter().enumerate() {
        println!(
          "dips to 1/{divisor}, run {round}, viewer {viewer}: {continuity:?}, republished {}, video held for the voice {} ms",
          keyframes.republished,
          run.held_for().as_millis()
        );
      }
    }
  }
}

/// An uplink with room for the share that drops to `1 / divisor` of it for [`SWING_LOW`] every
/// [`SWING_PERIOD`], after the first [`SETTLE`].
fn swinging_scenario(size: VideoResolution, divisor: u64) -> Scenario {
  let layers_bps: u64 = share_layers(size)
    .iter()
    .map(|layer| u64::from(layer.max_bitrate_bps))
    .sum();
  let dips = (0..)
    .map(|index| SETTLE + SWING_PERIOD * index)
    .take_while(|after| *after + SWING_LOW < SHARE)
    .map(|after| Dip {
      after,
      lasts: SWING_LOW,
      limit_bps: layers_bps / divisor,
    })
    .collect();
  Scenario {
    maybe_limit_bps: Some((layers_bps as f64 * LIMIT_SHARE) as u64),
    dips,
    viewers: LOSSY_VIEWERS,
    slide_intervals: SHORT_AND_LONG_SLIDES,
    ..Scenario::new(size)
  }
}

/// How long a swinging uplink stays low, and how often it drops.
const SWING_LOW: Duration = Duration::from_secs(3);
const SWING_PERIOD: Duration = Duration::from_secs(6);

/// A viewer's pictures: when, the capture timestamp, the size.
type Pictures = Vec<(Duration, u64, VideoResolution)>;

/// What a run shares, to whom, through which uplink.
struct Scenario {
  /// The shared screen's size.
  size: VideoResolution,
  maybe_limit_bps: Option<u64>,
  /// Stretches with a narrower uplink, in order.
  dips: Vec<Dip>,
  /// Loss and delay on the presenter's uplink, from the share's start.
  maybe_impairment: Option<Impairment>,
  /// How long the slides stay, in turn.
  slide_intervals: &'static [Duration],
  viewers: usize,
  /// The SFU asks the presenter for packets its uplink lost (#2189).
  is_uplink_repaired: bool,
}

impl Scenario {
  /// One viewer, slides every [`SLIDE_INTERVAL`], an uplink without a limit or loss.
  fn new(size: VideoResolution) -> Self {
    Self {
      size,
      maybe_limit_bps: None,
      dips: Vec::new(),
      maybe_impairment: None,
      slide_intervals: &[SLIDE_INTERVAL],
      viewers: 1,
      is_uplink_repaired: true,
    }
  }
}

/// What a viewer was told of the shared screen's track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenChange {
  /// A screen track started: its tile is built and waits for its first picture.
  Started(Duration),
  /// The track goes on with other layers (#2417): the tile and its picture stay.
  Changed(Duration),
  /// A track ended (only the screen's are watched): its tile goes.
  Ended(Duration),
}

/// What a viewer saw of the share after [`SETTLE`] (#2417).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Continuity {
  /// Screen tracks that started or ended: each one a tile built anew, the share stage gone
  /// meanwhile.
  restarts: usize,
  /// Times the track went on with other layers.
  changes: usize,
  /// The longest time between two pictures.
  longest_gap: Duration,
  /// Times without a picture longer than [`VISIBLE_GAP`].
  visible_gaps: usize,
  /// Times the picture's size changed.
  size_changes: usize,
}

/// A time without a new picture that a viewer notices on slides that change every second or two.
const VISIBLE_GAP: Duration = Duration::from_millis(2500);

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
  /// Each viewer's pictures.
  received: Vec<Pictures>,
  /// What each viewer was told of the screen share's track.
  screens: Vec<Vec<ScreenChange>>,
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
  /// When the presenter's video was held for its voice (#1871) and let go again.
  held: Vec<(Duration, bool)>,
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

/// Shares slides at `size` through an uplink limited to `maybe_limit_bps`, to one viewer.
async fn run(size: VideoResolution, maybe_limit_bps: Option<u64>, maybe_dip: Option<Dip>) -> Run {
  run_scenario(Scenario {
    maybe_limit_bps,
    dips: maybe_dip.into_iter().collect(),
    ..Scenario::new(size)
  })
  .await
}

/// Shares slides as `scenario` says.
async fn run_scenario(scenario: Scenario) -> Run {
  let Scenario {
    size,
    maybe_limit_bps,
    dips,
    maybe_impairment,
    slide_intervals,
    viewers: viewer_count,
    is_uplink_repaired,
  } = scenario;
  let mut sfu_config = SfuConfig::default();
  if !is_uplink_repaired {
    sfu_config.forwarding.uplink_repair.max_requests = 0;
  }
  let (port, certificate_hash) = start_sfu_with(sfu_config).await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let start = Instant::now();
  let relay = Relay::start(sfu, start);
  relay.to_sfu.set_limit(maybe_limit_bps);
  let devices: Vec<i64> = (1..=1 + i64::try_from(viewer_count).unwrap()).collect();
  let others =
    |device: i64| -> Vec<i64> { devices.iter().copied().filter(|other| *other != device).collect() };
  let mut presenter = join_with(relay.address, certificate_hash, 1, &others(1)).await;
  let mut viewers = Vec::new();
  for &device in &devices[1..] {
    viewers.push(join_with(sfu, certificate_hash, device, &others(device)).await);
  }
  wait_until_secured(&mut presenter).await;
  for viewer in &mut viewers {
    wait_until_secured(viewer).await;
  }
  pump_all_for(&mut presenter, &mut viewers, SOUND_ONLY).await;
  // The uplink turns lossy with the share: the call's setup is no part of the test.
  relay.to_sfu.set_impairment(maybe_impairment);

  let slides = SlideShow::new(start, size, slide_intervals);
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

  let received: Vec<Arc<Mutex<Pictures>>> = viewers.iter().map(|_| Arc::default()).collect();
  let mut screens: Vec<Vec<ScreenChange>> = viewers.iter().map(|_| Vec::new()).collect();
  let (mut uplink_estimates, mut layers_needed) = (Vec::new(), Vec::new());
  let (mut keyframe_requests, mut published, mut held) = (Vec::new(), Vec::new(), Vec::new());
  let share_start = tokio::time::Instant::now();
  let end = share_start + SHARE;
  let mut is_dipped: Option<u64> = None;
  while tokio::time::Instant::now() < end {
    let since = share_start.elapsed();
    let maybe_dip = dips
      .iter()
      .find(|dip| since >= dip.after && since < dip.after + dip.lasts);
    let dipped_to = maybe_dip.map(|dip| dip.limit_bps);
    if dipped_to != is_dipped {
      is_dipped = dipped_to;
      relay.to_sfu.set_limit(dipped_to.or(maybe_limit_bps));
    }
    match tokio::time::timeout(Duration::from_millis(5), presenter.next_event()).await {
      Ok(Some(CallEngineEvent::LayersNeeded { layers, .. })) => layers_needed.push((start.elapsed(), layers)),
      Ok(Some(CallEngineEvent::UplinkEstimate { bps, .. })) => uplink_estimates.push((start.elapsed(), bps)),
      Ok(Some(CallEngineEvent::KeyframeRequested { .. })) => keyframe_requests.push(start.elapsed()),
      Ok(Some(CallEngineEvent::VideoHeldForVoice(is_held))) => held.push((start.elapsed(), is_held)),
      Ok(Some(CallEngineEvent::VideoPublished(track))) if track.kind == VideoTrackKind::Screen => {
        published.push(start.elapsed());
      },
      _ => {},
    }
    for ((viewer, received), screens) in viewers.iter_mut().zip(&received).zip(&mut screens) {
      let track = match tokio::time::timeout(Duration::from_millis(5), viewer.next_event()).await {
        Ok(Some(CallEngineEvent::VideoPublished(track))) if track.kind == VideoTrackKind::Screen => track,
        Ok(Some(CallEngineEvent::VideoTrackChanged(track))) if track.kind == VideoTrackKind::Screen => {
          screens.push(ScreenChange::Changed(start.elapsed()));
          continue;
        },
        Ok(Some(CallEngineEvent::VideoUnpublished(_))) => {
          screens.push(ScreenChange::Ended(start.elapsed()));
          continue;
        },
        _ => continue,
      };
      screens.push(ScreenChange::Started(start.elapsed()));
      let sink = Arc::clone(received);
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
  for viewer in viewers {
    viewer.leave().await.unwrap();
  }
  let received = received
    .iter()
    .map(|received| received.lock().unwrap().clone())
    .collect();
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
    screens,
    shown,
    encoded,
    built,
    rate_changes,
    link,
    uplink_estimates,
    layers_needed,
    keyframe_requests,
    held,
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
      let maybe_delay = self.received[0]
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

  /// Per viewer and slide after [`SETTLE`]: how it went from its first picture out of the encoder
  /// (any layer) to the viewer.
  fn link_delays(&self) -> Vec<Arrival> {
    self
      .received
      .iter()
      .flat_map(|received| self.link_delays_to(received))
      .collect()
  }

  fn link_delays_to(&self, received: &Pictures) -> Vec<Arrival> {
    let settled = self.started_at + SETTLE;
    let in_window =
      |picture_us: u64, from_us: u64, to_us: u64| picture_us + 100 >= from_us && picture_us + 100 < to_us;
    let mut delays = Vec::new();
    for pair in self.changes.windows(2) {
      let [(changed_at, timestamp_us), (next_at, next_us)] = pair else {
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
      let maybe_shown_at = received
        .iter()
        .find(|(_, picture_us, _)| in_window(*picture_us, *timestamp_us, *next_us))
        .map(|(at, ..)| *at);
      let arrival = match maybe_shown_at {
        Some(shown_at) => Arrival::After(shown_at.saturating_sub(encoded_at)),
        // The presenter stopped sending video for its voice before the slide went out whole: the
        // voice guard's call, not the link's (#1871).
        None if self.is_held_between(encoded_at, *next_at) => Arrival::Held,
        None => Arrival::Lost,
      };
      delays.push(arrival);
    }
    delays
  }

  /// Per viewer: what it saw of the share after [`SETTLE`].
  fn continuity(&self) -> Vec<Continuity> {
    let settled = self.started_at + SETTLE;
    self
      .received
      .iter()
      .zip(&self.screens)
      .map(|(received, screens)| {
        let after_settle = |change: &&ScreenChange| match change {
          ScreenChange::Started(at) | ScreenChange::Changed(at) | ScreenChange::Ended(at) => *at >= settled,
        };
        let restarts = screens
          .iter()
          .filter(after_settle)
          .filter(|change| !matches!(change, ScreenChange::Changed(_)))
          .count();
        let changes = screens
          .iter()
          .filter(after_settle)
          .filter(|change| matches!(change, ScreenChange::Changed(_)))
          .count();
        let pictures: Vec<&(Duration, u64, VideoResolution)> =
          received.iter().filter(|(at, ..)| *at >= settled).collect();
        let gaps: Vec<Duration> = pictures
          .windows(2)
          .map(|pair| pair[1].0.saturating_sub(pair[0].0))
          .collect();
        // The share's end counts too: a picture that never came back is the longest gap of all.
        let tail = pictures
          .last()
          .map_or(SHARE, |(at, ..)| (self.started_at + SHARE).saturating_sub(*at));
        Continuity {
          restarts,
          changes,
          longest_gap: gaps.iter().copied().chain([tail]).max().unwrap_or(SHARE),
          visible_gaps: gaps
            .iter()
            .chain([&tail])
            .filter(|gap| **gap > VISIBLE_GAP)
            .count(),
          size_changes: pictures.windows(2).filter(|pair| pair[0].2 != pair[1].2).count(),
        }
      })
      .collect()
  }

  /// Whether the presenter's video was held for its voice at some time from `from` to `to`.
  fn is_held_between(&self, from: Duration, to: Duration) -> bool {
    let was_held_at_from = self
      .held
      .iter()
      .rev()
      .find(|(at, _)| *at <= from)
      .is_some_and(|(_, is_held)| *is_held);
    was_held_at_from
      || self
        .held
        .iter()
        .any(|(at, is_held)| *is_held && *at > from && *at < to)
  }

  /// Per second of the share: the uplink estimate's lowest, the layers needed and holds of the
  /// video for the voice that began, when anything of it changed.
  fn timeline(&self) -> String {
    let mut parts = Vec::new();
    let seconds = SHARE.as_secs();
    for second in 0..seconds {
      let from = self.started_at + Duration::from_secs(second);
      let to = from + Duration::from_secs(1);
      let within = |at: &Duration| *at >= from && *at < to;
      let lowest = self
        .uplink_estimates
        .iter()
        .filter(|(at, _)| within(at))
        .map(|(_, bps)| bps / 1000)
        .min();
      let layers: Vec<&Vec<u8>> = self
        .layers_needed
        .iter()
        .filter(|(at, _)| within(at))
        .map(|(_, layers)| layers)
        .collect();
      let held: Vec<bool> = self
        .held
        .iter()
        .filter(|(at, _)| within(at))
        .map(|(_, is_held)| *is_held)
        .collect();
      parts.push(format!("{second}: {lowest:?} {layers:?} {held:?}"));
    }
    parts.join(" | ")
  }

  /// How long the presenter's video was held for its voice after [`SETTLE`].
  fn held_for(&self) -> Duration {
    let settled = self.started_at + SETTLE;
    let mut total = Duration::ZERO;
    let mut maybe_since: Option<Duration> = None;
    for (at, is_held) in &self.held {
      match (maybe_since, *is_held) {
        (None, true) => maybe_since = Some((*at).max(settled)),
        (Some(since), false) => {
          total += at.saturating_sub(since);
          maybe_since = None;
        },
        _ => {},
      }
    }
    let end = self.started_at + SHARE;
    total + maybe_since.map_or(Duration::ZERO, |since| end.saturating_sub(since))
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
        "encoder to viewers (ms):     {}",
        self
          .link_delays()
          .iter()
          .map(|arrival| match arrival {
            Arrival::NotEncoded => "not encoded".to_owned(),
            Arrival::Lost => "-".to_owned(),
            Arrival::Held => "- (held)".to_owned(),
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
      format!(
        "timeline (s: estimate kbit/s, layers needed, held): {}",
        self.timeline()
      ),
      format!(
        "video held for the voice after the first {} s: {} ms in {} holds",
        SETTLE.as_secs(),
        self.held_for().as_millis(),
        self.held.iter().filter(|(_, is_held)| *is_held).count()
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

/// How a slide went from the encoder to the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrival {
  /// The encoder gave no picture of it (a machine too slow for every slide).
  NotEncoded,
  /// The viewer never showed it.
  Lost,
  /// The viewer never showed it, and the presenter held its video for its voice meanwhile.
  Held,
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
  /// How long the slides stay, in turn.
  intervals: &'static [Duration],
  maybe_running: Option<(std_mpsc::Sender<()>, JoinHandle<()>)>,
}

impl SlideShow {
  /// Slides drawn at the Retina size and shrunk to `size` where it is another.
  fn new(start: Instant, size: VideoResolution, intervals: &'static [Duration]) -> Self {
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
      intervals,
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
    let (start, intervals) = (self.start, self.intervals);
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
          stopped.recv_timeout(intervals[index % intervals.len()]),
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

/// Handles the calls' events for `duration`.
async fn pump_all_for(presenter: &mut NativeCall, viewers: &mut [NativeCall], duration: Duration) {
  let end = tokio::time::Instant::now() + duration;
  while tokio::time::Instant::now() < end {
    let _ = tokio::time::timeout(Duration::from_millis(5), presenter.next_event()).await;
    for viewer in viewers.iter_mut() {
      let _ = tokio::time::timeout(Duration::from_millis(5), viewer.next_event()).await;
    }
  }
}
