//! Captures from the xdg-desktop-portal like the desktop app does, and reports what arrived:
//! a check of the screen-share and camera paths on a real Wayland session (or a headless one,
//! see `apps/desktop/packaging/linux/portal-test/`).
//!
//! ```sh
//! cargo run -p teamcraft-media --features pipewire --example portal_capture -- screen
//! cargo run -p teamcraft-media --features pipewire --example portal_capture -- camera
//! ```
//!
//! Exits non-zero unless at least `--frames` frames (default 30) arrive within 20 seconds.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use teamcraft_media::capture_portal::PortalCapture;
use teamcraft_media::video::VideoResolution;
use teamcraft_media::video_capture::{CaptureFormat, VideoCapture, frame_queue};
use tracing::info;

const WANTED: CaptureFormat = CaptureFormat {
  resolution: VideoResolution {
    width: 1280,
    height: 720,
  },
  framerate: 30,
};
const DEADLINE: Duration = Duration::from_secs(20);

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt().with_target(false).init();
  let mut arguments = std::env::args().skip(1);
  let source = arguments.next().unwrap_or_else(|| "screen".into());
  let wanted_frames: usize = match (arguments.next().as_deref(), arguments.next()) {
    (Some("--frames"), Some(count)) => count.parse().context("--frames takes a number")?,
    _ => 30,
  };
  let mut capture = match source.as_str() {
    "screen" => {
      let (capture, maybe_token) = PortalCapture::screen(WANTED, None).context("screen portal")?;
      info!(has_restore_token = maybe_token.is_some(), "screen granted");
      capture
    },
    "camera" => PortalCapture::camera(WANTED).context("camera portal")?,
    other => bail!("unknown source {other}; use screen or camera"),
  };
  let format = capture.format();
  info!(
    width = format.resolution.width,
    height = format.resolution.height,
    "capturing"
  );
  let (frames, received) = frame_queue();
  capture.start(frames).context("starting the PipeWire stream")?;
  let start = Instant::now();
  let mut count = 0;
  let mut maybe_first = None;
  while count < wanted_frames && start.elapsed() < DEADLINE {
    if let Ok(frame) = received.recv_timeout(Duration::from_millis(500)) {
      count += 1;
      maybe_first.get_or_insert(frame.resolution());
    }
  }
  capture.stop();
  let elapsed = start.elapsed().as_secs_f64();
  info!(count, seconds = format!("{elapsed:.1}"), first = ?maybe_first, "done");
  if count < wanted_frames {
    bail!("only {count} of {wanted_frames} frames within {DEADLINE:?}");
  }
  Ok(())
}
