//! A screen share's layers arrive at about their bitrate also while text scrolls (#979): a Mac's
//! VideoToolbox sent a 1440p share's 720p layer at 9 Mbit/s for its 1.7, so a viewer on a weak
//! link, given that layer for its bitrate, got five times as much and stalled.
//!
//! The presenter shares a page of text that scrolls every frame, at 30 fps, with the
//! desktop's layers (`video::screen_layers`, detail mode) through the encoders the desktop uses
//! (`preferred_encoders`: hardware where the build and the machine have it, OpenH264 otherwise).
//! The viewer picks each layer in turn and measures what arrives of it through a real SFU.
//!
//! `measure_scrolled_screen_layers` (ignored) does the same for a 1440p share and prints the
//! numbers: `cargo test --release -p teamcraft-sfu --test suite measure_scrolled_screen_layers --
//! --ignored --nocapture` (add `--features teamcraft-media/gstreamer` for VideoToolbox).

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_media::video::{ScreenProfile, TOP_LAYER, VideoFrame, VideoResolution, screen_layers};
use teamcraft_media::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture};
use teamcraft_media::video_codec::{EncodedPicture, VideoCodecError, VideoEncoder};
use teamcraft_media::video_pipeline::{EncoderFactory, VideoSendConfig, preferred_encoders};
use teamcraft_proto::{
  AudioCodec, DeviceId, MediaCapabilities, MediaStreamId, ScreenContentMode, ScreenShareResolution,
  VideoCodec, VideoLayerInfo, VideoTrackKind,
};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use tokio::time::Instant;
use wtransport::Identity;

/// A 720p screen: two layers, 720p at 2 Mbit/s and 360p at 0.5 (small enough for OpenH264 in a
/// debug build on CI).
const HD: VideoResolution = VideoResolution {
  width: 1280,
  height: 720,
};
/// A 1440p screen: 1440p at 6.6 Mbit/s and 720p at 1.7, the layers of #979.
const QHD: VideoResolution = VideoResolution {
  width: 2560,
  height: 1440,
};
const FRAMERATE: u8 = 30;
/// How long a layer may take to arrive: the SFU switches on the layer's next keyframe, and the
/// downlink estimate needs a few seconds to allow a top layer.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);
/// Frames in a row on a layer that make it the one received.
const SETTLED_FRAMES: usize = 15;
/// How long a layer's bitrate is measured once it arrives.
const MEASURE: Duration = Duration::from_secs(4);
/// Rows the page scrolls per frame in the asserting test: whole pixels in every layer of a 720p
/// screen (the 360p layer is half the size). A fraction of a pixel in a layer makes OpenH264's
/// screen mode choose a keyframe on its own every few pictures (#2466): a probe of the encoder
/// alone, with no SFU in between, gives 10 keyframes in 13 pictures at 1.5 pixels of the layer
/// per picture and 1 in 124 at 1; no encoder option avoided it (scene-change detection off, rate
/// control mode, complexity, QP range, long-term reference all gave the same pictures). The keyframes
/// are no request of the SFU or the viewer, which the test asserts too ([`EncoderLog`]).
const WHOLE_PIXEL_ROWS: usize = 2;
/// Rows per frame of #979's measurement: a half pixel in the 360p layer, the content that made
/// VideoToolbox send five times a layer's bitrate.
const HALF_PIXEL_ROWS: usize = 1;
/// The most a layer may send of its bitrate: DAVE's few bytes per frame, the keyframes a switch
/// asks for, and few pictures to measure where OpenH264 skips most (it sent 96 to 117 % in a
/// debug build). VideoToolbox sent 1286 % before #979, 63 % after.
const MAX_SHARE: f64 = 1.5;
/// The most keyframes asked of one layer over the test: the first picture, the switch to it, a
/// repair or two.
const MAX_FORCED_KEYFRAMES: u32 = 4;

/// #979: each layer of a scrolled 720p screen arrives at most at about its bitrate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scrolled_text_arrives_at_about_each_layers_bitrate() {
  let (measured, encoders) = measure(HD, WHOLE_PIXEL_ROWS).await;
  for line in &measured {
    println!("{line}");
  }
  println!("encoders: {encoders:?}");
  for line in &measured {
    let received = line.received.as_ref().unwrap_or_else(|seen| {
      panic!(
        "layer {} never arrived (layers seen: {seen:?}); all: {measured:?}",
        line.layer.layer
      )
    });
    // OpenH264 holds the bitrate by skipping pictures (8 of the 360p layer's 120 in a debug
    // build); VideoToolbox sends them all.
    assert!(received.frames >= 4, "{line}");
    assert!(
      received.share(&line.layer) <= MAX_SHARE,
      "{line}; encoders: {encoders:?}"
    );
  }
  // What the SFU and the viewer may cause: one encoder per layer, built once and never again for
  // a rate, and the keyframes asked for (the first picture, the viewer's switch to the layer).
  // Keyframes the encoder chooses itself are the encoder's.
  for (width, counts) in &encoders {
    assert_eq!(counts.built, 1, "layer {width}: encoder rebuilt; {encoders:?}");
    assert_eq!(
      counts.rate_rebuilt, 0,
      "layer {width}: rebuilt for a rate; {encoders:?}"
    );
    assert!(
      counts.forced <= MAX_FORCED_KEYFRAMES,
      "layer {width}: keyframe storm; {encoders:?}"
    );
  }
}

/// Prints what arrives of each layer of a scrolled 1440p screen (#979's numbers).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement: prints the layers' bitrates, takes about a minute"]
async fn measure_scrolled_screen_layers() {
  for line in measure(QHD, HALF_PIXEL_ROWS).await.0 {
    println!("{line}");
  }
}

/// What arrived of one layer.
#[derive(Debug)]
struct Measured {
  layer: VideoLayerInfo,
  /// The layers that came instead, if this one never did.
  received: Result<Received, Vec<u8>>,
}

#[derive(Debug)]
struct Received {
  bytes: usize,
  frames: usize,
  seconds: f64,
}

impl Received {
  fn bitrate_bps(&self) -> f64 {
    self.bytes as f64 * 8.0 / self.seconds
  }

  fn share(&self, layer: &VideoLayerInfo) -> f64 {
    self.bitrate_bps() / f64::from(layer.max_bitrate_bps)
  }
}

impl std::fmt::Display for Measured {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let layer = &self.layer;
    write!(
      f,
      "layer {} {}x{} target {} kbit/s: ",
      layer.layer,
      layer.width,
      layer.height,
      layer.max_bitrate_bps / 1000
    )?;
    match &self.received {
      Ok(received) => write!(
        f,
        "{:.0} kbit/s ({:.0} %), {} frames in {:.1} s",
        received.bitrate_bps() / 1000.0,
        received.share(layer) * 100.0,
        received.frames,
        received.seconds
      ),
      Err(seen) => write!(f, "never arrived (layers seen: {seen:?})"),
    }
  }
}

/// Shares a scrolling screen of `size` and measures what arrives of each of its layers, lowest
/// first.
async fn measure(
  size: VideoResolution,
  rows_per_frame: usize,
) -> (Vec<Measured>, std::collections::BTreeMap<u16, Counts>) {
  let (port, certificate_hash) = start_sfu().await;
  let presenter = join(port, certificate_hash, 1, 2).await;
  let mut viewer = join(port, certificate_hash, 2, 1).await;
  let log = EncoderLog::default();
  let layers = screen_layers(
    size,
    FRAMERATE,
    ScreenContentMode::Detail,
    &ScreenShareResolution::Standard,
  );
  presenter
    .start_video(VideoSendConfig {
      kind: VideoTrackKind::Screen,
      capture: Box::new(ScrollingText::new(size, rows_per_frame)),
      layers: layers.clone(),
      maybe_layer_plan: None,
      encoders: counting(preferred_encoders(), log.clone()),
      maybe_filter: None,
      maybe_picture_tap: None,
      maybe_idle_refresh: VideoSendConfig::default_idle_refresh(&VideoTrackKind::Screen),
      // Stepping along the share policy like the desktop's share (#1777).
      maybe_screen_profile: Some(ScreenProfile {
        mode: ScreenContentMode::Detail,
        size: ScreenShareResolution::Standard,
      }),
    })
    .await
    .unwrap();
  let stream = receive_screen(&mut viewer, size).await;
  let mut measured = Vec::new();
  for layer in &layers {
    viewer.set_video_quality(stream, Some(layer.height));
    let received = match wait_for_layer(&mut viewer, stream, layer.layer).await {
      Ok(()) => Ok(receive(&mut viewer, stream, layer.layer).await),
      Err(seen) => Err(seen),
    };
    measured.push(Measured {
      layer: layer.clone(),
      received,
    });
  }
  presenter.stop_video(VideoTrackKind::Screen);
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();
  let encoders = std::mem::take(&mut *log.0.lock().unwrap());
  (measured, encoders)
}

/// The presenter's screen once the viewer knows it, received (encoded frames as events) in a
/// tile as large as the screen.
async fn receive_screen(viewer: &mut NativeCall, size: VideoResolution) -> MediaStreamId {
  let deadline = Instant::now() + STEP_TIMEOUT;
  while Instant::now() < deadline {
    let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(50), viewer.next_event()).await else {
      continue;
    };
    if let CallEngineEvent::VideoPublished(track) = event
      && track.kind == VideoTrackKind::Screen
    {
      viewer.set_video_shown(track.stream_id, size);
      viewer.subscribe_video(track.stream_id, Some(TOP_LAYER));
      return track.stream_id;
    }
  }
  panic!("the presenter's screen never showed up");
}

/// Waits until [`SETTLED_FRAMES`] frames in a row of `stream` came from `layer`; the layers that
/// came meanwhile otherwise.
async fn wait_for_layer(viewer: &mut NativeCall, stream: MediaStreamId, layer: u8) -> Result<(), Vec<u8>> {
  let deadline = Instant::now() + STEP_TIMEOUT;
  let mut in_a_row = 0;
  let mut seen: Vec<u8> = Vec::new();
  while Instant::now() < deadline {
    let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(50), viewer.next_event()).await else {
      continue;
    };
    let CallEngineEvent::VideoFrame(frame) = event else {
      continue;
    };
    if frame.stream_id != stream {
      continue;
    }
    let arrived = frame.layer.value();
    if seen.last() != Some(&arrived) {
      seen.push(arrived);
    }
    in_a_row = if arrived == layer { in_a_row + 1 } else { 0 };
    if in_a_row >= SETTLED_FRAMES {
      return Ok(());
    }
  }
  Err(seen)
}

/// What arrives of `layer` of `stream` over [`MEASURE`].
async fn receive(viewer: &mut NativeCall, stream: MediaStreamId, layer: u8) -> Received {
  let (mut bytes, mut frames) = (0, 0);
  let started = Instant::now();
  while started.elapsed() < MEASURE {
    let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(50), viewer.next_event()).await else {
      continue;
    };
    if let CallEngineEvent::VideoFrame(frame) = event
      && frame.stream_id == stream
      && frame.layer.value() == layer
    {
      bytes += frame.data.len();
      frames += 1;
    }
  }
  Received {
    bytes,
    frames,
    seconds: started.elapsed().as_secs_f64(),
  }
}

/// A page of text three screens tall that scrolls `rows_per_frame` pixels every frame, at
/// [`FRAMERATE`]: one row in a half-size layer is half a pixel, which an encoder can't follow by
/// moving blocks.
struct ScrollingText {
  size: VideoResolution,
  rows_per_frame: usize,
  /// Luma rows of the page.
  page: Arc<Vec<u8>>,
  maybe_running: Option<(std_mpsc::Sender<()>, JoinHandle<()>)>,
}

impl ScrollingText {
  fn new(size: VideoResolution, rows_per_frame: usize) -> Self {
    const LINE: usize = 19;
    let (width, height) = (usize::from(size.width), usize::from(size.height) * 3);
    let mut page = vec![235u8; width * height];
    let mut random = 0x2545_f491_4f6c_dd1du64;
    for top in (0..height - LINE).step_by(LINE) {
      for left in (width / 16..width - width / 16).step_by(7) {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        if random.is_multiple_of(7) {
          continue;
        }
        // A glyph of strokes a pixel wide, 5 by 9, like a Retina display's small text.
        for row in 0..9 {
          for column in 0..5 {
            if (random >> (row * 5 + column)) & 1 == 1 {
              page[(top + 5 + row) * width + left + column] = 40;
            }
          }
        }
      }
    }
    Self {
      size,
      rows_per_frame,
      page: Arc::new(page),
      maybe_running: None,
    }
  }
}

impl VideoCapture for ScrollingText {
  fn format(&self) -> CaptureFormat {
    CaptureFormat {
      resolution: self.size,
      framerate: FRAMERATE,
    }
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    let (stop, stopped) = std_mpsc::channel::<()>();
    let (page, size, rows_per_frame) = (Arc::clone(&self.page), self.size, self.rows_per_frame);
    let thread = std::thread::spawn(move || {
      let (width, height) = (usize::from(size.width), usize::from(size.height));
      let scroll_rows = page.len() / width - height;
      let period = Duration::from_secs(1) / u32::from(FRAMERATE);
      let start = std::time::Instant::now();
      let mut next = start;
      for index in 0usize.. {
        let top = index * rows_per_frame % scroll_rows * width;
        let mut data = Vec::with_capacity(width * height * 3 / 2);
        data.extend_from_slice(&page[top..top + width * height]);
        data.resize(width * height * 3 / 2, 128);
        let timestamp_us = u64::try_from(start.elapsed().as_micros()).unwrap();
        if !frames.send(VideoFrame::new(size, timestamp_us, data).unwrap()) {
          return;
        }
        next += period;
        let wait = next.saturating_duration_since(std::time::Instant::now());
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

async fn join(port: u16, certificate_hash: [u8; 32], device: i64, other: i64) -> NativeCall {
  let engine = CallEngineConfig {
    user_id: id(device),
    device_id: id(device),
    capabilities: MediaCapabilities {
      audio_codecs: vec![AudioCodec::Opus],
      video_encoders: vec![VideoCodec::H264],
      video_decoders: vec![VideoCodec::H264],
      max_simulcast_layers: 3,
      max_frame_fragments: teamcraft_proto::MAX_VIDEO_FRAME_FRAGMENTS,
      video_features: Vec::new(),
      audio_features: Vec::new(),
    },
    bwe: BweConfig::default(),
  };
  let sfu_url = format!(
    "https://{}{CALL_PATH}",
    SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
  );
  let mut config = NativeCallConfig::new(sfu_url, format!("dev:14:{device}:{device}"), engine);
  config.maybe_certificate_hash = Some(certificate_hash);
  config.maybe_trusted_roster = Some(vec![id::<DeviceId>(other)]);
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

/// What the presenter's encoders did, per layer width: pictures, keyframes by cause, encoders
/// built and rate changes.
#[derive(Default, Clone)]
struct EncoderLog(Arc<Mutex<std::collections::BTreeMap<u16, Counts>>>);

#[derive(Debug, Default)]
struct Counts {
  pictures: u32,
  built: u32,
  new_encoder: u32,
  forced: u32,
  chosen: u32,
  rate_in_place: u32,
  rate_rebuilt: u32,
}

fn counting(encoders: EncoderFactory, log: EncoderLog) -> EncoderFactory {
  Arc::new(move |layer, content| {
    log.0.lock().unwrap().entry(layer.width).or_default().built += 1;
    Ok(Box::new(Counting {
      inner: encoders(layer, content)?,
      width: layer.width,
      has_encoded: false,
      log: log.clone(),
    }) as Box<dyn VideoEncoder>)
  })
}

struct Counting {
  inner: Box<dyn VideoEncoder>,
  width: u16,
  has_encoded: bool,
  log: EncoderLog,
}

impl VideoEncoder for Counting {
  fn codec(&self) -> teamcraft_proto::VideoCodec {
    self.inner.codec()
  }

  fn name(&self) -> &'static str {
    self.inner.name()
  }

  fn encode(&mut self, frame: &VideoFrame, forced: bool) -> Result<Option<EncodedPicture>, VideoCodecError> {
    let maybe_picture = self.inner.encode(frame, forced)?;
    if let Some(picture) = &maybe_picture {
      let mut log = self.log.0.lock().unwrap();
      let counts = log.entry(self.width).or_default();
      counts.pictures += 1;
      if picture.is_keyframe {
        match (self.has_encoded, forced) {
          (false, _) => counts.new_encoder += 1,
          (true, true) => counts.forced += 1,
          (true, false) => counts.chosen += 1,
        }
      }
      self.has_encoded = true;
    }
    Ok(maybe_picture)
  }

  fn set_rate(&mut self, bitrate_bps: u32, framerate: u8) -> Result<bool, VideoCodecError> {
    let in_place = self.inner.set_rate(bitrate_bps, framerate)?;
    let mut log = self.log.0.lock().unwrap();
    let counts = log.entry(self.width).or_default();
    if in_place {
      counts.rate_in_place += 1;
    } else {
      counts.rate_rebuilt += 1;
    }
    Ok(in_place)
  }
}
