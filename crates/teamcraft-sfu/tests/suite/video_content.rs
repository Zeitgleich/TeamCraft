//! Pictures keep their content from the capture to the viewer (issue #683: shared screens and
//! cameras arrived black). The captures hand over what macOS delivers – packed BGRA from
//! ScreenCaptureKit, 4:2:2 YUYV (`yuvs`) from an AVFoundation camera – through the same conversion,
//! encoder, packetization, encryption and SFU as a call; every picture the viewer decodes has to
//! show what was captured, not black.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_media::capture_convert::{PixelFormat, RawPicture};
use teamcraft_media::picture_check::{PictureContent, PictureStats};
use teamcraft_media::video::{VideoFrame, VideoResolution, screen_layers, simulcast_layers};
use teamcraft_media::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture};
use teamcraft_media::video_pipeline::{VideoSendConfig, software_encoders};
use teamcraft_proto::{
  AudioCodec, DeviceId, MediaCapabilities, ScreenContentMode, VideoCodec, VideoTrackKind,
};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use wtransport::Identity;

const SIZE: VideoResolution = VideoResolution {
  width: 640,
  height: 360,
};
const FRAMERATE: u8 = 30;
/// Decoded pictures the viewer has to see.
const WANTED_PICTURES: usize = 15;
const DEADLINE: Duration = Duration::from_secs(10);
/// Luma PSNR a decoded picture keeps at least against what was captured (lossy, not black: a
/// black picture against these scenes is below 10 dB).
const MIN_PSNR_DB: f64 = 25.0;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shared_screen_reaches_the_viewer_with_its_content() {
  let capture = SyntheticCapture::new(PixelFormat::Bgrx);
  let layers = screen_layers(SIZE, FRAMERATE, ScreenContentMode::Detail);
  check_content_arrives(VideoTrackKind::Screen, capture, layers).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_reaches_the_viewer_with_its_content() {
  let capture = SyntheticCapture::new(PixelFormat::Yuyv);
  let layers = simulcast_layers(SIZE, FRAMERATE, 1_000_000, 3);
  check_content_arrives(VideoTrackKind::Camera, capture, layers).await;
}

async fn check_content_arrives(
  kind: VideoTrackKind,
  capture: SyntheticCapture,
  layers: Vec<teamcraft_proto::VideoLayerInfo>,
) {
  let (port, certificate_hash) = start_sfu().await;
  let mut presenter = join(port, certificate_hash, 1, 2).await;
  let mut viewer = join(port, certificate_hash, 2, 1).await;
  let captured = Arc::clone(&capture.captured);
  let config = VideoSendConfig {
    maybe_layer_plan: None,
    kind: kind.clone(),
    maybe_idle_refresh: VideoSendConfig::default_idle_refresh(&kind),
    capture: Box::new(capture),
    layers,
    encoders: software_encoders(),
    maybe_filter: None,
  };
  presenter.start_video(config).await.unwrap();

  let decoded: Arc<Mutex<Vec<VideoFrame>>> = Arc::default();
  let deadline = tokio::time::Instant::now() + DEADLINE;
  while tokio::time::Instant::now() < deadline && decoded.lock().unwrap().len() < WANTED_PICTURES {
    // The presenter's events must be read for its call to go on.
    let _ = tokio::time::timeout(Duration::from_millis(5), presenter.next_event()).await;
    if let Ok(Some(CallEngineEvent::VideoPublished(track))) =
      tokio::time::timeout(Duration::from_millis(5), viewer.next_event()).await
    {
      assert_eq!(track.kind, kind);
      let sink = Arc::clone(&decoded);
      viewer.watch_video(
        track.stream_id,
        None,
        Box::new(move |frame| sink.lock().unwrap().push(frame)),
      );
    }
  }
  presenter.stop_video(kind.clone());
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();

  let decoded = decoded.lock().unwrap();
  assert!(
    decoded.len() >= WANTED_PICTURES,
    "{kind:?}: {} pictures decoded",
    decoded.len()
  );
  let captured = captured.lock().unwrap();
  for picture in decoded.iter() {
    let stats = PictureStats::of(picture);
    assert_eq!(stats.content(), PictureContent::Detailed, "{kind:?}: {stats:?}");
    let source = captured_at(&captured, picture.timestamp_us())
      .scaled(picture.resolution())
      .unwrap();
    let psnr = luma_psnr(&source, picture);
    assert!(
      psnr >= MIN_PSNR_DB,
      "{kind:?} at {} µs, {:?}: PSNR {psnr:.1} dB",
      picture.timestamp_us(),
      picture.resolution()
    );
  }
}

/// Draws a scene in a raw format the way a device delivers it, converts it like the platform
/// captures do (`RawPicture::to_i420`), and keeps what it sent for the comparison.
struct SyntheticCapture {
  format: PixelFormat,
  captured: Arc<Mutex<BTreeMap<u64, VideoFrame>>>,
  maybe_running: Option<(std_mpsc::Sender<()>, JoinHandle<()>)>,
}

impl SyntheticCapture {
  fn new(format: PixelFormat) -> Self {
    Self {
      format,
      captured: Arc::default(),
      maybe_running: None,
    }
  }
}

impl VideoCapture for SyntheticCapture {
  fn format(&self) -> CaptureFormat {
    CaptureFormat {
      resolution: SIZE,
      framerate: FRAMERATE,
    }
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    let (stop, stopped) = std_mpsc::channel();
    let (format, captured) = (self.format, Arc::clone(&self.captured));
    let thread = std::thread::spawn(move || {
      let period = Duration::from_secs(1) / u32::from(FRAMERATE);
      for index in 0u64.. {
        let timestamp_us = index * 33_333;
        let raw = draw(format, index);
        let picture = RawPicture {
          format,
          width: u32::from(SIZE.width),
          height: u32::from(SIZE.height),
          stride: 0,
          data: &raw,
        };
        let frame = picture.to_i420(timestamp_us).unwrap();
        captured.lock().unwrap().insert(timestamp_us, frame.clone());
        if !frames.send(frame) {
          return;
        }
        if !matches!(
          stopped.recv_timeout(period),
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

/// A desktop-like scene: a coloured gradient, a light "window" with dark lines of "text", and a
/// box that moves with `index`, so pictures differ.
fn draw(format: PixelFormat, index: u64) -> Vec<u8> {
  let (width, height) = (usize::from(SIZE.width), usize::from(SIZE.height));
  let box_x = (index as usize * 8) % (width - 64);
  let rgb = |x: usize, y: usize| -> [u8; 3] {
    if (box_x..box_x + 64).contains(&x) && (40..104).contains(&y) {
      return [230, 60, 40];
    }
    if (160..600).contains(&x) && (140..340).contains(&y) {
      let is_text = (y - 140) % 16 < 3 && (x - 160) % 40 < 30;
      return if is_text { [20, 20, 30] } else { [245, 245, 240] };
    }
    [(x * 255 / width) as u8, (y * 255 / height) as u8, 128]
  };
  match format {
    PixelFormat::Bgrx => (0..height)
      .flat_map(|y| (0..width).map(move |x| (x, y)))
      .flat_map(|(x, y)| {
        let [r, g, b] = rgb(x, y);
        [b, g, r, 255]
      })
      .collect(),
    PixelFormat::Yuyv => (0..height)
      .flat_map(|y| (0..width).step_by(2).map(move |x| (x, y)))
      .flat_map(|(x, y)| {
        let (y0, u0, v0) = yuv(rgb(x, y));
        let (y1, u1, v1) = yuv(rgb(x + 1, y));
        [y0, average(u0, u1), y1, average(v0, v1)]
      })
      .collect(),
    other => panic!("no scene for {other:?}"),
  }
}

/// BT.601 limited range, as cameras deliver it.
fn yuv([r, g, b]: [u8; 3]) -> (u8, u8, u8) {
  let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
  let y = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
  let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
  let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
  (y as u8, u.clamp(0, 255) as u8, v.clamp(0, 255) as u8)
}

fn average(a: u8, b: u8) -> u8 {
  ((u16::from(a) + u16::from(b)).div_ceil(2)) as u8
}

/// The captured frame a decoded picture came from: its timestamp went through the 90 kHz RTP
/// clock, which rounds.
fn captured_at(captured: &BTreeMap<u64, VideoFrame>, timestamp_us: u64) -> &VideoFrame {
  captured
    .range(timestamp_us.saturating_sub(100)..=timestamp_us + 100)
    .map(|(_, frame)| frame)
    .next()
    .unwrap_or_else(|| panic!("no captured frame at {timestamp_us} µs"))
}

fn luma_psnr(a: &VideoFrame, b: &VideoFrame) -> f64 {
  let (ya, yb) = (a.planes().0, b.planes().0);
  let mse = ya
    .iter()
    .zip(yb)
    .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
    .sum::<f64>()
    / ya.len() as f64;
  10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10()
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
