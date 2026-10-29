//! A camera that looks busy to an encoder, shared by the suite's camera tests: a lit background
//! with texture, a head and a hand that move, and sensor noise in every picture, so OpenH264 uses
//! its whole bitrate (a test pattern of flat bars needs a fraction of it).

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use teamcraft_media::video::{VideoFrame, VideoResolution};
use teamcraft_media::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture};

/// What the busy camera delivers: 720p at 30 fps.
pub const HD: VideoResolution = VideoResolution {
  width: 1280,
  height: 720,
};
pub const FRAMERATE: u8 = 30;
/// Distinct pictures the camera cycles through (one second of motion).
pub const CAMERA_PICTURES: usize = 30;

/// A camera that looks busy to an encoder: a lit background with texture, a head and a hand that
/// move, and sensor noise in every picture. The pictures are drawn once and repeat every second.
pub struct BusyCamera {
  pictures: Arc<Vec<VideoFrame>>,
  /// Which picture was delivered at which timestamp.
  shown: Arc<Mutex<BTreeMap<u64, usize>>>,
  start: Instant,
  maybe_running: Option<(std_mpsc::Sender<()>, JoinHandle<()>)>,
}

impl BusyCamera {
  pub fn new(start: Instant) -> Self {
    let pictures = (0..CAMERA_PICTURES).map(draw_camera_picture).collect();
    Self {
      pictures: Arc::new(pictures),
      shown: Arc::default(),
      start,
      maybe_running: None,
    }
  }

  /// The pictures it cycles through, by index.
  pub fn pictures(&self) -> Arc<Vec<VideoFrame>> {
    Arc::clone(&self.pictures)
  }

  /// Which picture was delivered at which timestamp.
  pub fn shown(&self) -> Arc<Mutex<BTreeMap<u64, usize>>> {
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
