//! A shared screen full of text is readable at the viewer within seconds (issue #941: a macOS
//! share arrived blurry, "a low resolution scaled up", and browser text was unreadable).
//!
//! The presenter shares a still, text-heavy screen the way ScreenCaptureKit delivers one: a picture
//! only when something changes (a blinking cursor), at the size of a Retina display fitted into
//! the detail mode's 1440p, with the layers the desktop plans for it. The viewer watches it like
//! the desktop (top layer asked for) through a real SFU, after the call carried only sound for a
//! while and a lossy moment cut the SFU's estimate of the viewer's downlink. Every decoded picture
//! is compared with what was shared, at the shared size (a lower layer is scaled up, as the
//! viewer's screen would show it).
//!
//! `measure_screen_share_sharpness` (ignored) prints the timeline of the first 20 seconds per
//! scenario: layer, luma PSNR and the encoders' output per layer.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use teamcraft_media::audio_io::AudioSource;
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_media::capture_convert::{PixelFormat, RawPicture};
use teamcraft_media::video::{VideoFrame, VideoResolution, screen_layers, screen_limits};
use teamcraft_media::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture};
use teamcraft_media::video_codec::{EncodedPicture, VideoCodecError, VideoEncoder};
use teamcraft_media::video_pipeline::{EncoderFactory, VideoSendConfig, preferred_encoders};
use teamcraft_proto::{
  AudioCodec, DeviceId, MediaCapabilities, ScreenContentMode, VideoCodec, VideoTrackKind,
};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use tokio::net::UdpSocket;
use wtransport::Identity;

/// A 14" Retina display (3024×1964) fitted into the detail mode's 1440p, as ScreenCaptureKit
/// captures it.
const SHARED: VideoResolution = VideoResolution {
  width: 2216,
  height: 1440,
};
/// A blinking cursor: the only change on a page being read.
const CHANGE_INTERVAL: Duration = Duration::from_millis(500);
/// Distinct pictures the screen cycles through (cursor on and off, a few typed characters).
const STATES: usize = 6;
/// How long the call carries only sound before the share.
const SOUND_ONLY: Duration = Duration::from_secs(4);
/// The lossy moment on the viewer's downlink (every other packet from the SFU lost).
const LOSSY_MOMENT: Duration = Duration::from_secs(2);
/// The top layer has to reach the viewer within this time of the share's start.
const SHARP_DEADLINE: Duration = Duration::from_secs(8);
/// Luma PSNR (dB) of the viewer's picture against the shared screen, at the shared size, that
/// keeps this text readable. The half-size layer scaled up stays far below.
const MIN_SHARP_PSNR_DB: f64 = 30.0;
/// Bounds the steps before the share starts (securing, the lossy moment).
const STEP_DEADLINE: Duration = Duration::from_secs(10);

/// The viewer gets the top layer of a text-heavy share within seconds, sharp enough to read,
/// although the call carried only sound before and a lossy moment cut the SFU's estimate of the
/// viewer's downlink.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shared_text_screen_is_sharp_at_the_viewer_within_seconds() {
  let timeline = run(
    Scenario::LossyDownlinkBefore,
    SHARP_DEADLINE + Duration::from_secs(2),
  )
  .await;
  let maybe_sharp_at = timeline
    .decoded
    .iter()
    .find(|picture| picture.resolution == SHARED && picture.psnr_db >= MIN_SHARP_PSNR_DB)
    .map(|picture| picture.at);
  let summary = timeline.summary();
  println!("{summary}");
  let Some(sharp_at) = maybe_sharp_at else {
    panic!("no sharp picture of the top layer reached the viewer:\n{summary}");
  };
  assert!(
    sharp_at <= SHARP_DEADLINE,
    "the first sharp picture came after {sharp_at:?}:\n{summary}"
  );
  // Once sharp, it stays: no picture of the top layer falls back to blur.
  let blurred: Vec<&Decoded> = timeline
    .decoded
    .iter()
    .filter(|picture| picture.at > sharp_at && picture.psnr_db < MIN_SHARP_PSNR_DB)
    .collect();
  assert!(
    blurred.is_empty(),
    "pictures after the first sharp one are blurred again ({:?} …):\n{summary}",
    blurred.first()
  );
}

/// Prints what the viewer gets of a text-heavy share over its first 20 seconds, per scenario.
/// `cargo test --release -p teamcraft-sfu --test suite measure_screen_share_sharpness --
/// --ignored --nocapture` (add `--features teamcraft-media/gstreamer` for VideoToolbox).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement: prints a timeline, takes about two minutes"]
async fn measure_screen_share_sharpness() {
  for scenario in [
    Scenario::Fresh,
    Scenario::SoundBefore,
    Scenario::LossyDownlinkBefore,
    Scenario::LossyUplinkBefore,
  ] {
    let timeline = run(scenario, Duration::from_secs(20)).await;
    println!("== {scenario:?}\n{}", timeline.summary());
  }
}

/// What happens in the call before the share starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
  /// The share starts as soon as the call is secured.
  Fresh,
  /// Sound only for [`SOUND_ONLY`] first.
  SoundBefore,
  /// Sound only, with a lossy moment on the viewer's downlink.
  LossyDownlinkBefore,
  /// Sound only, with a lossy moment on the presenter's uplink (#935).
  LossyUplinkBefore,
}

/// What the viewer decoded and the presenter encoded, timed from the share's start.
#[derive(Debug, Default)]
struct Timeline {
  decoded: Vec<Decoded>,
  encoded: Vec<Encoded>,
  /// What the presenter heard: the layers someone receives, its uplink estimate.
  heard: Vec<(Duration, Heard)>,
}

#[derive(Debug, Clone)]
enum Heard {
  LayersNeeded(Vec<u8>),
  UplinkEstimate(u32),
}

#[derive(Debug, Clone, Copy)]
struct Decoded {
  at: Duration,
  resolution: VideoResolution,
  /// Luma PSNR against the shared picture, at the shared size.
  psnr_db: f64,
  /// Luma PSNR against the shared picture scaled to the decoded size: what the encoder kept.
  own_psnr_db: f64,
}

#[derive(Debug, Clone, Copy)]
struct Encoded {
  at: Duration,
  width: u16,
  bytes: usize,
  is_keyframe: bool,
}

/// Runs `scenario` and records `duration` from the share's start.
async fn run(scenario: Scenario, duration: Duration) -> Timeline {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let presenter_relay = Relay::start(sfu).await;
  let viewer_relay = Relay::start(sfu).await;
  let mut presenter = join(presenter_relay.address, certificate_hash, 1, 2).await;
  let mut viewer = join(viewer_relay.address, certificate_hash, 2, 1).await;
  wait_until_secured(&mut presenter).await;
  wait_until_secured(&mut viewer).await;

  if scenario != Scenario::Fresh {
    pump_for(&mut presenter, &mut viewer, SOUND_ONLY / 2).await;
    let maybe_relay = match scenario {
      Scenario::LossyDownlinkBefore => Some(&viewer_relay.to_client_losing),
      Scenario::LossyUplinkBefore => Some(&presenter_relay.to_sfu_losing),
      _ => None,
    };
    if let Some(losing) = maybe_relay {
      losing.store(true, Ordering::Relaxed);
      pump_for(&mut presenter, &mut viewer, LOSSY_MOMENT).await;
      losing.store(false, Ordering::Relaxed);
    }
    pump_for(&mut presenter, &mut viewer, SOUND_ONLY / 2).await;
  }

  let screen = TextScreen::new();
  let sources = Arc::clone(&screen.sources);
  let shown = Arc::clone(&screen.shown);
  let start = Instant::now();
  let encoded: Arc<Mutex<Vec<Encoded>>> = Arc::default();
  let (limit, framerate) = screen_limits(ScreenContentMode::Detail);
  let config = VideoSendConfig {
    kind: VideoTrackKind::Screen,
    layers: screen_layers(limit, framerate, ScreenContentMode::Detail),
    capture: Box::new(screen),
    encoders: counting(preferred_encoders(), start, Arc::clone(&encoded)),
    maybe_filter: None,
    maybe_idle_refresh: VideoSendConfig::default_idle_refresh(&VideoTrackKind::Screen),
  };
  presenter.start_video(config).await.unwrap();

  let decoded: Arc<Mutex<Vec<Decoded>>> = Arc::default();
  let mut heard = Vec::new();
  let end = tokio::time::Instant::now() + duration;
  while tokio::time::Instant::now() < end {
    match tokio::time::timeout(Duration::from_millis(5), presenter.next_event()).await {
      Ok(Some(CallEngineEvent::LayersNeeded { layers, .. })) => {
        heard.push((start.elapsed(), Heard::LayersNeeded(layers)));
      },
      Ok(Some(CallEngineEvent::UplinkEstimate { bps, .. })) => {
        heard.push((start.elapsed(), Heard::UplinkEstimate(bps)));
      },
      _ => {},
    }
    if let Ok(Some(CallEngineEvent::VideoPublished(track))) =
      tokio::time::timeout(Duration::from_millis(5), viewer.next_event()).await
      && track.kind == VideoTrackKind::Screen
    {
      let (sink, sources, shown) = (Arc::clone(&decoded), Arc::clone(&sources), Arc::clone(&shown));
      viewer.watch_video(
        track.stream_id,
        None,
        Box::new(move |frame| {
          let at = start.elapsed();
          let source = shown_at(&shown.lock().unwrap(), frame.timestamp_us());
          let psnr_db = psnr_at_shared_size(&sources[source], &frame);
          let own_psnr_db = luma_psnr(&sources[source].scaled(frame.resolution()).unwrap(), &frame);
          sink.lock().unwrap().push(Decoded {
            at,
            resolution: frame.resolution(),
            psnr_db,
            own_psnr_db,
          });
        }),
      );
    }
  }
  presenter.stop_video(VideoTrackKind::Screen);
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();
  let decoded = decoded.lock().unwrap().clone();
  let encoded = encoded.lock().unwrap().clone();
  Timeline {
    decoded,
    encoded,
    heard,
  }
}

impl Timeline {
  /// One line per second: the viewer's layers, pictures and PSNR; the presenter's output per
  /// layer (kbit/s, keyframes).
  fn summary(&self) -> String {
    let seconds = self
      .decoded
      .iter()
      .map(|picture| picture.at.as_secs())
      .chain(self.encoded.iter().map(|picture| picture.at.as_secs()))
      .max()
      .unwrap_or(0);
    let mut lines = vec![
      "  s  viewer (pictures size: mean/min PSNR dB)  |  sent per layer width: kbit/s (keyframes)  |  layers needed, uplink kbit/s (low-high)"
        .to_string(),
    ];
    for second in 0..=seconds {
      let mut by_size: BTreeMap<(u16, u16), Vec<(f64, f64)>> = BTreeMap::new();
      for picture in self
        .decoded
        .iter()
        .filter(|picture| picture.at.as_secs() == second)
      {
        by_size
          .entry((picture.resolution.width, picture.resolution.height))
          .or_default()
          .push((picture.psnr_db, picture.own_psnr_db));
      }
      let viewer: Vec<String> = by_size
        .iter()
        .map(|((width, height), psnrs)| {
          let count = psnrs.len() as f64;
          let mean = psnrs.iter().map(|(shared, _)| shared).sum::<f64>() / count;
          let min = psnrs
            .iter()
            .map(|(shared, _)| *shared)
            .fold(f64::INFINITY, f64::min);
          let own = psnrs.iter().map(|(_, own)| own).sum::<f64>() / count;
          format!(
            "{}× {width}x{height}: {mean:.1}/{min:.1} (own {own:.1})",
            psnrs.len()
          )
        })
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
      let sent: Vec<String> = by_layer
        .iter()
        .map(|(width, (bytes, keyframes))| format!("{width}: {} ({keyframes})", bytes * 8 / 1000))
        .collect();
      // The layers asked for in order, and the uplink estimate's range between them.
      let mut presenter = Vec::new();
      let mut maybe_range: Option<(u32, u32)> = None;
      let close_range =
        |presenter: &mut Vec<String>, maybe_range: &mut Option<(u32, u32)>| match maybe_range.take() {
          Some((low, high)) if low == high => presenter.push(format!("{}", low / 1000)),
          Some((low, high)) => presenter.push(format!("{}-{}", low / 1000, high / 1000)),
          None => {},
        };
      for (_, heard) in self.heard.iter().filter(|(at, _)| at.as_secs() == second) {
        match heard {
          Heard::LayersNeeded(layers) => {
            close_range(&mut presenter, &mut maybe_range);
            presenter.push(format!("{layers:?}"));
          },
          Heard::UplinkEstimate(bps) => {
            let (low, high) = maybe_range.unwrap_or((*bps, *bps));
            maybe_range = Some((low.min(*bps), high.max(*bps)));
          },
        }
      }
      close_range(&mut presenter, &mut maybe_range);
      lines.push(format!(
        "{second:3}  {:<46} |  {:<32} |  {}",
        viewer.join(", "),
        sent.join(", "),
        presenter.join(" ")
      ));
    }
    lines.join("\n")
  }
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

/// A still page of text like a browser shows it on a Retina display, with a blinking cursor and
/// a few typed characters: ScreenCaptureKit delivers a picture only when something changes.
struct TextScreen {
  /// The distinct pictures, drawn once.
  sources: Arc<Vec<VideoFrame>>,
  /// Which picture was delivered at which timestamp.
  shown: Arc<Mutex<BTreeMap<u64, usize>>>,
  maybe_running: Option<(std_mpsc::Sender<()>, JoinHandle<()>)>,
}

impl TextScreen {
  fn new() -> Self {
    let sources = (0..STATES)
      .map(|state| {
        let bgrx = draw_page(state);
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
      shown: Arc::default(),
      maybe_running: None,
    }
  }
}

impl VideoCapture for TextScreen {
  /// As ScreenCaptureKit's capture: the largest picture asked for, not the display's size.
  fn format(&self) -> CaptureFormat {
    let (resolution, framerate) = screen_limits(ScreenContentMode::Detail);
    CaptureFormat {
      resolution,
      framerate,
    }
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    let (stop, stopped) = std_mpsc::channel::<()>();
    let (sources, shown) = (Arc::clone(&self.sources), Arc::clone(&self.shown));
    let thread = std::thread::spawn(move || {
      let start = Instant::now();
      for index in 0usize.. {
        let state = index % STATES;
        let timestamp_us = u64::try_from(start.elapsed().as_micros()).unwrap();
        shown.lock().unwrap().insert(timestamp_us, state);
        if !frames.send(sources[state].clone().with_timestamp_us(timestamp_us)) {
          return;
        }
        if !matches!(
          stopped.recv_timeout(CHANGE_INTERVAL),
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

/// Packed BGRX of the page in `state`: a toolbar, a sidebar, body text at Retina size (2 px
/// strokes), a block of small print (1 px strokes), blue links, and a line being typed with a
/// cursor.
fn draw_page(state: usize) -> Vec<u8> {
  let (width, height) = (usize::from(SHARED.width), usize::from(SHARED.height));
  let mut page = vec![[250u8, 250, 250]; width * height];
  fill(&mut page, width, (0, 0, width, 76), [222, 222, 226]);
  fill(&mut page, width, (0, 76, 360, height), [240, 241, 244]);
  // Toolbar: an address field.
  fill(&mut page, width, (440, 18, 1500, 58), [255, 255, 255]);
  text(&mut page, width, (460, 26), 2, 60, [60, 60, 70], 1);
  // Sidebar entries.
  for row in 0..30 {
    text(
      &mut page,
      width,
      (32, 110 + row * 40),
      2,
      22,
      [40, 40, 50],
      100 + row,
    );
  }
  // Body: paragraphs of Retina-size text, a link per paragraph.
  for row in 0..36 {
    let color = if row % 6 == 5 { [20, 90, 200] } else { [25, 25, 30] };
    text(&mut page, width, (420, 110 + row * 30), 2, 140, color, 200 + row);
  }
  // Small print.
  for row in 0..12 {
    text(
      &mut page,
      width,
      (420, 1220 + row * 15),
      1,
      280,
      [70, 70, 80],
      400 + row,
    );
  }
  // The line being typed, and the cursor.
  let typed = 2 + state / 2;
  text(&mut page, width, (420, 1190), 2, typed, [25, 25, 30], 600);
  if state.is_multiple_of(2) {
    fill(
      &mut page,
      width,
      (420 + typed * 12, 1188, 422 + typed * 12, 1206),
      [0, 0, 0],
    );
  }
  page.into_iter().flat_map(|[r, g, b]| [b, g, r, 255]).collect()
}

fn fill(page: &mut [[u8; 3]], width: usize, (x0, y0, x1, y1): (usize, usize, usize, usize), color: [u8; 3]) {
  for y in y0..y1 {
    page[y * width + x0..y * width + x1].fill(color);
  }
}

/// `count` pseudo-random 5×7 glyphs (with word gaps) at `scale`, from `seed`.
fn text(
  page: &mut [[u8; 3]],
  width: usize,
  (x, y): (usize, usize),
  scale: usize,
  count: usize,
  color: [u8; 3],
  seed: usize,
) {
  let mut random = (seed as u64)
    .wrapping_mul(6_364_136_223_846_793_005)
    .wrapping_add(1_442_695_040_888_963_407);
  for index in 0..count {
    random = random
      .wrapping_mul(6_364_136_223_846_793_005)
      .wrapping_add(1_442_695_040_888_963_407);
    if (random >> 60) < 2 {
      continue; // a space
    }
    let glyph = random >> 20;
    let left = x + index * 6 * scale;
    for row in 0..7 {
      for column in 0..5 {
        // Glyphs keep a vertical stem, like most letters.
        let is_ink = column == 0 || (glyph >> (row * 5 + column)) & 1 == 1;
        if !is_ink {
          continue;
        }
        for dy in 0..scale {
          let start = (y + row * scale + dy) * width + left + column * scale;
          page[start..start + scale].fill(color);
        }
      }
    }
  }
}

/// The picture shown at `timestamp_us`: the last one delivered up to then (a refresh of a still
/// screen repeats it with a later timestamp; RTP's 90 kHz clock rounds).
fn shown_at(shown: &BTreeMap<u64, usize>, timestamp_us: u64) -> usize {
  shown
    .range(..=timestamp_us + 100)
    .next_back()
    .map(|(_, state)| *state)
    .unwrap_or_else(|| panic!("nothing shown up to {timestamp_us} µs"))
}

/// Luma PSNR of `decoded` against `source`, with `decoded` scaled to the source's size.
fn psnr_at_shared_size(source: &VideoFrame, decoded: &VideoFrame) -> f64 {
  luma_psnr(source, &decoded.scaled(source.resolution()).unwrap())
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

/// Reads both calls' events (they must be read for the calls to go on) for `duration`.
async fn pump_for(presenter: &mut NativeCall, viewer: &mut NativeCall, duration: Duration) {
  let end = tokio::time::Instant::now() + duration;
  while tokio::time::Instant::now() < end {
    let _ = tokio::time::timeout(Duration::from_millis(5), presenter.next_event()).await;
    let _ = tokio::time::timeout(Duration::from_millis(5), viewer.next_event()).await;
  }
}

/// Relays one client's UDP to the SFU and back; told to, it loses every other packet in either
/// direction.
struct Relay {
  address: SocketAddr,
  to_sfu_losing: Arc<AtomicBool>,
  to_client_losing: Arc<AtomicBool>,
}

impl Relay {
  async fn start(sfu: SocketAddr) -> Self {
    let client_side = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    let sfu_side = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    sfu_side.connect(sfu).await.unwrap();
    let address = client_side.local_addr().unwrap();
    let (to_sfu_losing, to_client_losing) =
      (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let client: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
    let (from_client, to_sfu, known_client, losing) = (
      Arc::clone(&client_side),
      Arc::clone(&sfu_side),
      Arc::clone(&client),
      Arc::clone(&to_sfu_losing),
    );
    tokio::spawn(async move {
      let mut buffer = vec![0; 65_536];
      let mut is_odd = false;
      while let Ok((len, from)) = from_client.recv_from(&mut buffer).await {
        *known_client.lock().unwrap() = Some(from);
        is_odd = !is_odd;
        if losing.load(Ordering::Relaxed) && is_odd {
          continue;
        }
        let _ = to_sfu.send(&buffer[..len]).await;
      }
    });
    let losing = Arc::clone(&to_client_losing);
    tokio::spawn(async move {
      let mut buffer = vec![0; 65_536];
      let mut is_odd = false;
      while let Ok(len) = sfu_side.recv(&mut buffer).await {
        is_odd = !is_odd;
        if losing.load(Ordering::Relaxed) && is_odd {
          continue;
        }
        let maybe_client = *client.lock().unwrap();
        if let Some(client) = maybe_client {
          let _ = client_side.send_to(&buffer[..len], client).await;
        }
      }
    });
    Self {
      address,
      to_sfu_losing,
      to_client_losing,
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

async fn join(sfu: SocketAddr, certificate_hash: [u8; 32], device: i64, other: i64) -> NativeCall {
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
  config.maybe_trusted_roster = Some(vec![id::<DeviceId>(other)]);
  // The call's sound: what both estimates measure before the share.
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
