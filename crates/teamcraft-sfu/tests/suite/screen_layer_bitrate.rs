//! A screen share's layers arrive at about their bitrate also while text scrolls (#979): a Mac's
//! VideoToolbox sent a 1440p share's 720p layer at 9 Mbit/s for its 1.7, so a viewer on a weak
//! link, given that layer for its bitrate, got five times as much and stalled.
//!
//! The presenter shares a page of text that scrolls a pixel every frame, at 30 fps, with the
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
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_media::video::{TOP_LAYER, VideoFrame, VideoResolution, screen_layers};
use teamcraft_media::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture};
use teamcraft_media::video_pipeline::{VideoSendConfig, preferred_encoders};
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
/// The most a layer may send of its bitrate: DAVE's few bytes per frame, the keyframes a switch
/// asks for, and few pictures to measure where OpenH264 skips most (it sent 96 to 117 % in a
/// debug build). VideoToolbox sent 1286 % before #979, 63 % after.
const MAX_SHARE: f64 = 1.5;

/// #979: each layer of a scrolled 720p screen arrives at most at about its bitrate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scrolled_text_arrives_at_about_each_layers_bitrate() {
  let measured = measure(HD).await;
  for line in &measured {
    println!("{line}");
  }
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
    assert!(received.share(&line.layer) <= MAX_SHARE, "{line}");
  }
}

/// Prints what arrives of each layer of a scrolled 1440p screen (#979's numbers).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement: prints the layers' bitrates, takes about a minute"]
async fn measure_scrolled_screen_layers() {
  for line in measure(QHD).await {
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
async fn measure(size: VideoResolution) -> Vec<Measured> {
  let (port, certificate_hash) = start_sfu().await;
  let presenter = join(port, certificate_hash, 1, 2).await;
  let mut viewer = join(port, certificate_hash, 2, 1).await;
  let layers = screen_layers(
    size,
    FRAMERATE,
    ScreenContentMode::Detail,
    &ScreenShareResolution::Standard,
  );
  presenter
    .start_video(VideoSendConfig {
      kind: VideoTrackKind::Screen,
      capture: Box::new(ScrollingText::new(size)),
      layers: layers.clone(),
      maybe_layer_plan: None,
      encoders: preferred_encoders(),
      maybe_filter: None,
      maybe_idle_refresh: VideoSendConfig::default_idle_refresh(&VideoTrackKind::Screen),
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
      layer: *layer,
      received,
    });
  }
  presenter.stop_video(VideoTrackKind::Screen);
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();
  measured
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

/// A page of text three screens tall that scrolls a pixel every frame, at [`FRAMERATE`]: in a
/// half-size layer half a pixel, which an encoder can't follow by moving blocks.
struct ScrollingText {
  size: VideoResolution,
  /// Luma rows of the page.
  page: Arc<Vec<u8>>,
  maybe_running: Option<(std_mpsc::Sender<()>, JoinHandle<()>)>,
}

impl ScrollingText {
  fn new(size: VideoResolution) -> Self {
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
    let (page, size) = (Arc::clone(&self.page), self.size);
    let thread = std::thread::spawn(move || {
      let (width, height) = (usize::from(size.width), usize::from(size.height));
      let scroll_rows = page.len() / width - height;
      let period = Duration::from_secs(1) / u32::from(FRAMERATE);
      let start = std::time::Instant::now();
      let mut next = start;
      for index in 0usize.. {
        let top = index % scroll_rows * width;
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
