//! Cameras through V4L2 (`/dev/video*`), the Linux fallback when the camera portal isn't
//! available (no portal service, or a desktop without camera support in it).
//!
//! The format is negotiated once at open with the shared rule (`capture_negotiate`): the closest
//! size, preferring uncompressed formats over MJPEG at equal size (no decode cost); when the driver
//! refuses it, the driver's current format is used if it can be converted.

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
use crate::capture_negotiate::{OfferedMode, choose_mode};
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
    let offers = offers(&device, wanted.framerate)?;
    let (chosen, actual) = match choose_mode(&offers, wanted) {
      Some(chosen) => {
        let requested = Format::new(chosen.width, chosen.height, fourcc_of(chosen.pixel_format));
        match device.set_format(&requested) {
          Ok(actual) => (chosen, actual),
          Err(error) => {
            warn!(path = %path.display(), %error, "camera refused the chosen format");
            current_format(&device, &path)?
          },
        }
      },
      // Nothing listed we know: whatever the driver is set to, if we can convert it.
      None => current_format(&device, &path)?,
    };
    let fourcc = fourcc_of(chosen.pixel_format);
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

/// The driver's own format as a fallback; an error if it isn't convertible.
fn current_format(device: &Device, path: &std::path::Path) -> Result<(OfferedMode, Format), CaptureError> {
  let format = device.format().map_err(|error| device_error(path, &error))?;
  let pixel_format = pixel_format_of(format.fourcc)
    .ok_or_else(|| CaptureError::UnsupportedFormat(format!("{}: no mode we can convert", path.display())))?;
  let mode = OfferedMode {
    pixel_format,
    width: format.width,
    height: format.height,
    framerate: 0,
  };
  Ok((mode, format))
}

/// The modes the camera lists in formats we convert. V4L2 frame intervals are not enumerated, so
/// every mode carries the wanted rate (the rate is set separately when the stream starts).
fn offers(device: &Device, framerate: u8) -> Result<Vec<OfferedMode>, CaptureError> {
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
        offers.push(OfferedMode {
          pixel_format,
          width: discrete.width,
          height: discrete.height,
          framerate: u32::from(framerate),
        });
      }
    }
  }
  Ok(offers)
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
    // `choose_mode` never returns the packed RGB formats on V4L2 (they aren't listed).
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
}
