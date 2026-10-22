//! Cameras through V4L2 (`/dev/video*`), the Linux fallback when the camera portal isn't
//! available (no portal service, or a desktop without camera support in it).
//!
//! The format is negotiated once at open: the largest size up to the wanted one, preferring
//! uncompressed YUYV/NV12 over MJPEG at equal size (no decode cost), then the driver's choice.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{info, warn};
use v4l::buffer::Type;
use v4l::io::traits::CaptureStream;
use v4l::prelude::MmapStream;
use v4l::video::Capture;
use v4l::video::capture::Parameters;
use v4l::{Device, Format, FourCC};

use crate::capture_convert::{PixelFormat, RawPicture};
use crate::video::VideoResolution;
use crate::video_capture::{CameraInfo, CaptureError, CaptureFormat, FrameSender, VideoCapture};

/// Kernel buffers in flight.
const BUFFER_COUNT: u32 = 4;
/// How long a read may block before the thread looks at its stop flag.
const READ_TIMEOUT: Duration = Duration::from_millis(200);

/// One V4L2 camera with a negotiated format.
pub struct V4l2Camera {
  path: PathBuf,
  format: CaptureFormat,
  pixel_format: PixelFormat,
  fourcc: FourCC,
  maybe_running: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
}

/// One size a camera offers in one pixel format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Offer {
  pixel_format: PixelFormat,
  width: u32,
  height: u32,
}

/// Video capture devices (metadata nodes and other non-capture nodes left out).
pub fn list_cameras() -> Vec<CameraInfo> {
  v4l::context::enum_devices()
    .into_iter()
    .filter(|node| {
      Device::with_path(node.path())
        .and_then(|device| device.query_caps())
        .is_ok_and(|caps| caps.capabilities.contains(v4l::capability::Flags::VIDEO_CAPTURE))
    })
    .map(|node| CameraInfo {
      id: node.path().display().to_string(),
      name: node.name().unwrap_or_else(|| node.path().display().to_string()),
    })
    .collect()
}

impl V4l2Camera {
  /// Opens `maybe_id` (a [`CameraInfo::id`]) or the first camera, negotiating the format closest
  /// to `wanted`.
  pub fn open(maybe_id: Option<&str>, wanted: CaptureFormat) -> Result<Self, CaptureError> {
    let path = match maybe_id {
      Some(id) => PathBuf::from(id),
      None => list_cameras()
        .first()
        .map(|camera| PathBuf::from(&camera.id))
        .ok_or(CaptureError::NoDevice("camera"))?,
    };
    let device = Device::with_path(&path).map_err(|error| device_error(&path, &error))?;
    let offers = offers(&device)?;
    let chosen = choose(&offers, wanted.resolution).ok_or_else(|| {
      CaptureError::UnsupportedFormat(format!("{}: no YUYV, NV12 or MJPEG mode", path.display()))
    })?;
    let fourcc = fourcc_of(chosen.pixel_format);
    let actual = device
      .set_format(&Format::new(chosen.width, chosen.height, fourcc))
      .map_err(|error| device_error(&path, &error))?;
    let resolution = VideoResolution {
      width: u16::try_from(actual.width & !1)
        .map_err(|_| CaptureError::UnsupportedFormat("too wide".into()))?,
      height: u16::try_from(actual.height & !1)
        .map_err(|_| CaptureError::UnsupportedFormat("too high".into()))?,
    };
    info!(path = %path.display(), ?chosen, "camera format");
    Ok(Self {
      path,
      format: CaptureFormat {
        resolution,
        framerate: wanted.framerate,
      },
      pixel_format: chosen.pixel_format,
      fourcc,
      maybe_running: None,
    })
  }
}

impl VideoCapture for V4l2Camera {
  fn format(&self) -> CaptureFormat {
    self.format
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    self.stop();
    let device = Device::with_path(&self.path).map_err(|error| device_error(&self.path, &error))?;
    let resolution = self.format.resolution;
    device
      .set_format(&Format::new(
        u32::from(resolution.width),
        u32::from(resolution.height),
        self.fourcc,
      ))
      .map_err(|error| device_error(&self.path, &error))?;
    if let Err(error) = device.set_params(&Parameters::with_fps(u32::from(self.format.framerate))) {
      // Not every driver lets the rate be chosen; it then runs at its default.
      warn!(%error, "camera frame rate not set");
    }
    let format = device
      .format()
      .map_err(|error| device_error(&self.path, &error))?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let pixel_format = self.pixel_format;
    let path = self.path.clone();
    let thread = std::thread::Builder::new()
      .name("v4l2-camera".into())
      .spawn(move || {
        let mut stream = match MmapStream::with_buffers(&device, Type::VideoCapture, BUFFER_COUNT) {
          Ok(stream) => stream,
          Err(error) => {
            warn!(%error, path = %path.display(), "camera stream");
            return;
          },
        };
        stream.set_timeout(READ_TIMEOUT);
        let start = Instant::now();
        while !thread_stop.load(Ordering::Relaxed) {
          let (data, _) = match stream.next() {
            Ok(buffer) => buffer,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(error) => {
              warn!(%error, path = %path.display(), "camera stopped");
              return;
            },
          };
          let picture = RawPicture {
            format: pixel_format,
            width: format.width,
            height: format.height,
            stride: format.stride as usize,
            data,
          };
          let timestamp_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
          match picture.to_i420(timestamp_us) {
            Ok(frame) => {
              if !frames.send(frame) {
                return;
              }
            },
            Err(error) => warn!(%error, "camera frame dropped"),
          }
        }
      })
      .map_err(|error| CaptureError::Device(error.to_string()))?;
    self.maybe_running = Some((stop, thread));
    Ok(())
  }

  fn stop(&mut self) {
    if let Some((stop, thread)) = self.maybe_running.take() {
      stop.store(true, Ordering::Relaxed);
      if thread.join().is_err() {
        warn!("camera thread panicked");
      }
    }
  }
}

impl Drop for V4l2Camera {
  fn drop(&mut self) {
    self.stop();
  }
}

fn offers(device: &Device) -> Result<Vec<Offer>, CaptureError> {
  let mut offers = Vec::new();
  for description in device
    .enum_formats()
    .map_err(|error| CaptureError::Device(error.to_string()))?
  {
    let Some(pixel_format) = pixel_format_of(description.fourcc) else {
      continue;
    };
    let Ok(sizes) = device.enum_framesizes(description.fourcc) else {
      continue;
    };
    for size in sizes {
      for discrete in size.size.to_discrete() {
        offers.push(Offer {
          pixel_format,
          width: discrete.width,
          height: discrete.height,
        });
      }
    }
  }
  Ok(offers)
}

/// The largest mode not above `wanted` (or the smallest, if all are larger); at equal size
/// uncompressed formats win.
fn choose(offers: &[Offer], wanted: VideoResolution) -> Option<Offer> {
  let fits =
    |offer: &&Offer| offer.width <= u32::from(wanted.width) && offer.height <= u32::from(wanted.height);
  let rank = |offer: &Offer| {
    let cost = match offer.pixel_format {
      PixelFormat::Yuyv | PixelFormat::Nv12 | PixelFormat::I420 => 0,
      _ => 1,
    };
    (
      u64::from(offer.width) * u64::from(offer.height),
      std::cmp::Reverse(cost),
    )
  };
  offers
    .iter()
    .filter(fits)
    .max_by_key(|offer| rank(offer))
    .or_else(|| {
      offers
        .iter()
        .min_by_key(|offer| u64::from(offer.width) * u64::from(offer.height))
    })
    .copied()
}

fn pixel_format_of(fourcc: FourCC) -> Option<PixelFormat> {
  match &fourcc.repr {
    b"YUYV" => Some(PixelFormat::Yuyv),
    b"NV12" => Some(PixelFormat::Nv12),
    b"YU12" => Some(PixelFormat::I420),
    b"MJPG" => Some(PixelFormat::Mjpeg),
    _ => None,
  }
}

fn fourcc_of(format: PixelFormat) -> FourCC {
  FourCC::new(match format {
    PixelFormat::Yuyv => b"YUYV",
    PixelFormat::Nv12 => b"NV12",
    PixelFormat::I420 => b"YU12",
    PixelFormat::Mjpeg | PixelFormat::Bgrx | PixelFormat::Rgbx => b"MJPG",
  })
}

fn device_error(path: &std::path::Path, error: &std::io::Error) -> CaptureError {
  match error.kind() {
    std::io::ErrorKind::NotFound => CaptureError::NoDevice("camera"),
    std::io::ErrorKind::PermissionDenied => CaptureError::Denied("camera"),
    _ => CaptureError::Device(format!("{}: {error}", path.display())),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const HD: VideoResolution = VideoResolution {
    width: 1280,
    height: 720,
  };

  #[test]
  fn largest_fitting_mode_wins_and_raw_beats_mjpeg_at_equal_size() {
    let offers = [
      offer(PixelFormat::Yuyv, 640, 480),
      offer(PixelFormat::Mjpeg, 1280, 720),
      offer(PixelFormat::Yuyv, 1280, 720),
      offer(PixelFormat::Mjpeg, 1920, 1080),
    ];
    assert_eq!(choose(&offers, HD), Some(offer(PixelFormat::Yuyv, 1280, 720)));
    let mjpeg_only_hd = [
      offer(PixelFormat::Yuyv, 640, 480),
      offer(PixelFormat::Mjpeg, 1280, 720),
    ];
    assert_eq!(
      choose(&mjpeg_only_hd, HD),
      Some(offer(PixelFormat::Mjpeg, 1280, 720))
    );
  }

  #[test]
  fn too_large_cameras_give_their_smallest_mode() {
    let offers = [
      offer(PixelFormat::Mjpeg, 3840, 2160),
      offer(PixelFormat::Mjpeg, 1920, 1080),
    ];
    assert_eq!(choose(&offers, HD), Some(offer(PixelFormat::Mjpeg, 1920, 1080)));
    assert_eq!(choose(&[], HD), None);
  }

  #[test]
  fn fourccs_map_both_ways() {
    for format in [
      PixelFormat::Yuyv,
      PixelFormat::Nv12,
      PixelFormat::I420,
      PixelFormat::Mjpeg,
    ] {
      assert_eq!(pixel_format_of(fourcc_of(format)), Some(format));
    }
    assert_eq!(pixel_format_of(FourCC::new(b"H264")), None);
  }

  fn offer(pixel_format: PixelFormat, width: u32, height: u32) -> Offer {
    Offer {
      pixel_format,
      width,
      height,
    }
  }
}
