//! Captures the camera or a display like the macOS desktop app does, encodes and decodes the
//! pictures, and reports what they show: a check of the camera and screen-share paths on a real
//! Mac (issue #683: video that arrives black).
//!
//! ```sh
//! cargo run -p teamcraft-media --features avfoundation,screencapturekit,gstreamer,openh264-source \
//!   --example macos_capture -- camera [--seconds 5] [--save DIR]
//! cargo run -p teamcraft-media --features avfoundation,screencapturekit,gstreamer,openh264-source \
//!   --example macos_capture -- screen [--seconds 5] [--save DIR]
//! ```
//!
//! `camera` asks for the camera permission if it was never asked; `screen` captures the main
//! display without the picker and needs the Screen Recording permission of the app it runs in
//! (the terminal). Without `gstreamer` the pictures are encoded with OpenH264 only. `--save`
//! writes the first and last captured and decoded pictures as JPEG.
//!
//! Exits non-zero when no picture with content was captured, or the decoded pictures lost it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use teamcraft_media::picture_check::{PictureContent, PictureStats};
use teamcraft_media::video::{VideoFrame, VideoResolution, screen_layers, simulcast_layers};
use teamcraft_media::video_capture::{CaptureFormat, VideoCapture, frame_queue};
use teamcraft_media::video_codec::{OpenH264Decoder, SimulcastEncoder, VideoContent, VideoDecoder};
use teamcraft_media::video_jpeg::encode_jpeg;
use teamcraft_media::video_pipeline::preferred_encoders;
use teamcraft_proto::{ScreenContentMode, ScreenShareResolution};
use tracing::{info, warn};

const CAMERA: CaptureFormat = CaptureFormat {
  resolution: VideoResolution {
    width: 1280,
    height: 720,
  },
  framerate: 30,
};
const SCREEN: CaptureFormat = CaptureFormat {
  resolution: VideoResolution {
    width: 2560,
    height: 1440,
  },
  framerate: 30,
};
const JPEG_QUALITY: u8 = 85;

/// What the pictures of one stage showed.
#[derive(Debug, Default)]
struct Tally {
  black: usize,
  flat: usize,
  detailed: usize,
  maybe_first_detailed: Option<usize>,
}

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt().with_target(false).init();
  let mut arguments = std::env::args().skip(1);
  let source = arguments.next().unwrap_or_else(|| "camera".into());
  let mut seconds = 5u64;
  let mut maybe_save: Option<PathBuf> = None;
  while let Some(argument) = arguments.next() {
    match (argument.as_str(), arguments.next()) {
      ("--seconds", Some(value)) => seconds = value.parse().context("--seconds takes a number")?,
      ("--save", Some(value)) => maybe_save = Some(PathBuf::from(value)),
      (other, _) => bail!("unknown argument {other}"),
    }
  }
  let (mut capture, content): (Box<dyn VideoCapture>, VideoContent) = match source.as_str() {
    "camera" => (open_camera()?, VideoContent::Camera),
    "screen" => (open_screen()?, VideoContent::Screen),
    other => bail!("unknown source {other}; use camera or screen"),
  };
  let frames = capture_frames(capture.as_mut(), Duration::from_secs(seconds))?;
  let captured = tally(&frames);
  info!(?captured, "captured pictures");
  let decoded = encode_and_decode(&frames, content)?;
  let after_decoding = tally(&decoded);
  info!(?after_decoding, "pictures after encoding and decoding");
  if let Some(directory) = maybe_save {
    save(&directory, "captured", &frames)?;
    save(&directory, "decoded", &decoded)?;
  }
  if captured.detailed == 0 {
    bail!("no captured picture shows anything: {captured:?}");
  }
  if after_decoding.detailed == 0 {
    bail!("the pictures lost their content in encoding or decoding: {after_decoding:?}");
  }
  Ok(())
}

#[cfg(target_os = "macos")]
fn open_camera() -> anyhow::Result<Box<dyn VideoCapture>> {
  let cameras = teamcraft_media::capture_nokhwa::list_cameras();
  for camera in &cameras {
    info!(id = %camera.id, name = %camera.name, kind = ?camera.kind, "camera");
  }
  let camera = teamcraft_media::capture_nokhwa::NokhwaCamera::open(None, CAMERA).context("camera")?;
  Ok(Box::new(camera))
}

#[cfg(target_os = "macos")]
fn open_screen() -> anyhow::Result<Box<dyn VideoCapture>> {
  use teamcraft_media::capture_screencapturekit::{DisplaySource, ScreenCaptureKitScreen};

  let screen = ScreenCaptureKitScreen::display(DisplaySource::Main, SCREEN).context("main display")?;
  Ok(Box::new(screen))
}

/// Other platforms build the example (`--all-features`) but have neither capture.
#[cfg(not(target_os = "macos"))]
fn open_camera() -> anyhow::Result<Box<dyn VideoCapture>> {
  bail!("AVFoundation cameras ({CAMERA:?}) exist on macOS only")
}

#[cfg(not(target_os = "macos"))]
fn open_screen() -> anyhow::Result<Box<dyn VideoCapture>> {
  bail!("ScreenCaptureKit ({SCREEN:?}) exists on macOS only")
}

fn capture_frames(capture: &mut dyn VideoCapture, duration: Duration) -> anyhow::Result<Vec<VideoFrame>> {
  let (sender, received) = frame_queue();
  capture.start(sender).context("starting the capture")?;
  let start = Instant::now();
  let mut frames = Vec::new();
  while start.elapsed() < duration {
    if let Ok(frame) = received.recv_timeout(Duration::from_millis(500)) {
      let stats = PictureStats::of(&frame);
      if frames.is_empty() {
        info!(
          after_ms = start.elapsed().as_millis(),
          resolution = ?frame.resolution(),
          "first picture"
        );
      }
      info!(
        index = frames.len(),
        at_ms = frame.timestamp_us() / 1000,
        content = ?stats.content(),
        ?stats,
        "picture"
      );
      frames.push(frame);
    }
  }
  capture.stop();
  let rate = frames.len() as f64 / start.elapsed().as_secs_f64();
  info!(
    count = frames.len(),
    fps = format!("{rate:.1}"),
    "capture stopped"
  );
  Ok(frames)
}

/// Encodes every picture as the call would (hardware first, OpenH264 as the fallback) and decodes
/// the largest layer with OpenH264, as receivers without WebCodecs do.
fn encode_and_decode(frames: &[VideoFrame], content: VideoContent) -> anyhow::Result<Vec<VideoFrame>> {
  let Some(first) = frames.first() else {
    return Ok(Vec::new());
  };
  let layers = match content {
    VideoContent::Camera => simulcast_layers(first.resolution(), 30, 1_500_000, 3),
    VideoContent::Screen => screen_layers(
      first.resolution(),
      30,
      ScreenContentMode::Detail,
      &ScreenShareResolution::Standard,
    ),
  };
  let top_layer = layers.iter().map(|layer| layer.layer).max().unwrap_or(0);
  let encoders = preferred_encoders();
  let mut simulcast = SimulcastEncoder::new(&layers, |layer| encoders(layer, content))?;
  let mut decoder = OpenH264Decoder::new()?;
  let mut decoded = Vec::new();
  for frame in frames {
    for picture in simulcast.encode(frame)? {
      if picture.layer.value() != top_layer {
        continue;
      }
      match decoder.decode(&picture.data, frame.timestamp_us()) {
        Ok(Some(picture)) => decoded.push(picture),
        Ok(None) => {},
        Err(error) => warn!(%error, "picture not decoded"),
      }
    }
  }
  Ok(decoded)
}

fn tally(frames: &[VideoFrame]) -> Tally {
  let mut tally = Tally::default();
  for (index, frame) in frames.iter().enumerate() {
    match PictureStats::of(frame).content() {
      PictureContent::Black => tally.black += 1,
      PictureContent::Flat => tally.flat += 1,
      PictureContent::Detailed => {
        tally.detailed += 1;
        tally.maybe_first_detailed.get_or_insert(index);
      },
    }
  }
  tally
}

fn save(directory: &std::path::Path, stage: &str, frames: &[VideoFrame]) -> anyhow::Result<()> {
  std::fs::create_dir_all(directory)?;
  for (name, maybe_frame) in [("first", frames.first()), ("last", frames.last())] {
    if let Some(frame) = maybe_frame {
      let path = directory.join(format!("{stage}-{name}.jpg"));
      std::fs::write(&path, encode_jpeg(frame, JPEG_QUALITY)?)?;
      info!(path = %path.display(), "saved");
    }
  }
  Ok(())
}
