//! Cameras on macOS (AVFoundation) and Windows (Media Foundation), via `nokhwa` (a safe wrapper;
//! our crates have no `unsafe` for Objective-C or COM calls). Behind the `avfoundation` feature on
//! macOS and the `mediafoundation` feature on Windows.
//!
//! Untested: written and type-checked on Linux against nokhwa's V4L2 backend (macOS) and
//! cross-checked with `cargo xwin` (Windows), never run on a Mac or a Windows machine. The macOS
//! app's Info.plist needs `NSCameraUsageDescription`, or macOS denies access silently.

use std::time::Instant;

use nokhwa::Camera;
use nokhwa::NokhwaError;
use nokhwa::utils::{
  ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution,
};
use tracing::{info, warn};

use crate::camera_thread::{CameraReader, CameraThread};
use crate::capture_convert::{PixelFormat, RawPicture};
use crate::capture_negotiate::{OfferedMode, choose_mode};
use crate::video::VideoFrame;
use crate::video_capture::{CameraInfo, CaptureError, CaptureFormat, FrameSender, VideoCapture};

/// Frame formats `RawPicture::to_i420` converts.
const SUPPORTED_FORMATS: &[FrameFormat] = &[FrameFormat::NV12, FrameFormat::YUYV, FrameFormat::MJPEG];

/// A camera opened through nokhwa (AVFoundation on macOS, Media Foundation on Windows).
pub struct NokhwaCamera {
  index: CameraIndex,
  format: CaptureFormat,
  maybe_running: Option<CameraThread>,
}

/// An open nokhwa camera, read on its camera thread. nokhwa's read blocks until a frame comes, for
/// good when none does: [`CameraThread`] stops it with a bounded wait.
struct NokhwaReader(Camera);

/// Cameras the system reports.
pub fn list_cameras() -> Vec<CameraInfo> {
  nokhwa::query(ApiBackend::Auto)
    .map(|cameras| {
      cameras
        .into_iter()
        .map(|camera| CameraInfo {
          id: camera.index().to_string(),
          name: camera.human_name(),
        })
        .collect()
    })
    .unwrap_or_default()
}

impl NokhwaCamera {
  /// `maybe_id` is a [`CameraInfo::id`] (the camera's index); `None` opens the first camera.
  pub fn open(maybe_id: Option<&str>, wanted: CaptureFormat) -> Result<Self, CaptureError> {
    ensure_camera_permission()?;
    let index = match maybe_id {
      Some(id) => CameraIndex::String(id.to_owned()),
      None => CameraIndex::Index(0),
    };
    Ok(Self {
      index,
      format: wanted,
      maybe_running: None,
    })
  }
}

impl VideoCapture for NokhwaCamera {
  fn format(&self) -> CaptureFormat {
    self.format
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    self.stop();
    let index = self.index.clone();
    let wanted = self.format;
    // nokhwa's camera isn't `Send` on every platform: it is opened on the camera thread.
    let open = move || open_negotiated(index, wanted).map(NokhwaReader);
    self.maybe_running = Some(CameraThread::start("camera", open, frames)?);
    Ok(())
  }

  fn stop(&mut self) {
    if let Some(running) = self.maybe_running.take() {
      running.stop();
    }
  }
}

impl Drop for NokhwaCamera {
  fn drop(&mut self) {
    self.stop();
  }
}

impl CameraReader for NokhwaReader {
  fn read(&mut self, start: Instant) -> Result<Option<VideoFrame>, CaptureError> {
    let buffer = self
      .0
      .frame()
      .map_err(|error| CaptureError::Device(error.to_string()))?;
    let pixel_format = pixel_format_of(buffer.source_frame_format())
      .ok_or_else(|| CaptureError::UnsupportedFormat(format!("{:?}", buffer.source_frame_format())))?;
    let resolution = buffer.resolution();
    let picture = RawPicture {
      format: pixel_format,
      width: resolution.width(),
      height: resolution.height(),
      stride: 0,
      data: buffer.buffer(),
    };
    let timestamp_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
    match picture.to_i420(timestamp_us) {
      Ok(frame) => Ok(Some(frame)),
      Err(error) => {
        warn!(%error, "camera frame dropped");
        Ok(None)
      },
    }
  }

  fn close(&mut self) {
    if let Err(error) = self.0.stop_stream() {
      warn!(%error, "camera not stopped cleanly");
    }
  }
}

/// How long the macOS camera permission dialog may stay open.
#[cfg(target_os = "macos")]
const PERMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// macOS: asks for camera access unless already granted. Without it AVFoundation opens the
/// camera but never delivers a frame, so the call would send nothing and say nothing (#683).
#[cfg(target_os = "macos")]
fn ensure_camera_permission() -> Result<(), CaptureError> {
  if nokhwa::nokhwa_check() {
    return Ok(());
  }
  let (answer, answer_rx) = std::sync::mpsc::channel();
  let answer = std::sync::Mutex::new(answer);
  nokhwa::nokhwa_initialize(move |is_granted| {
    // The receiver is gone once the wait below gave up; nothing to tell then.
    let _ = answer
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .send(is_granted);
  });
  match answer_rx.recv_timeout(PERMISSION_TIMEOUT) {
    Ok(true) => Ok(()),
    Ok(false) | Err(_) => {
      warn!("camera access not granted");
      Err(CaptureError::Denied("camera"))
    },
  }
}

/// Windows asks no permission here (the system's privacy settings apply to the device open).
#[cfg(not(target_os = "macos"))]
fn ensure_camera_permission() -> Result<(), CaptureError> {
  Ok(())
}

/// Opens the camera in the mode closest to `wanted` among those it offers, instead of demanding one
/// fixed pixel format (AVFoundation cameras often have no NV12).
fn open_negotiated(index: CameraIndex, wanted: CaptureFormat) -> Result<Camera, CaptureError> {
  let device = |error: NokhwaError| CaptureError::Device(error.to_string());
  // Any mode we can convert gets the device open; the real choice follows once its modes are known.
  let any_mode = RequestedFormat::with_formats(RequestedFormatType::None, SUPPORTED_FORMATS);
  let mut camera = Camera::new(index, any_mode).map_err(device)?;
  let offered: Vec<OfferedMode> = camera
    .compatible_camera_formats()
    .map_err(device)?
    .into_iter()
    .filter_map(|mode| {
      Some(OfferedMode {
        pixel_format: pixel_format_of(mode.format())?,
        width: mode.width(),
        height: mode.height(),
        framerate: mode.frame_rate(),
      })
    })
    .collect();
  // No mode we can name: keep the one the device opened with, which is one we convert.
  if let Some(chosen) = choose_mode(&offered, wanted) {
    let exact = CameraFormat::new(
      Resolution::new(chosen.width, chosen.height),
      frame_format_of(chosen.pixel_format),
      chosen.framerate,
    );
    info!(width = chosen.width, height = chosen.height, framerate = chosen.framerate, format = ?chosen.pixel_format, "camera mode chosen");
    if let Err(error) = camera.set_camera_requset(RequestedFormat::with_formats(
      RequestedFormatType::Exact(exact),
      SUPPORTED_FORMATS,
    )) {
      warn!(%error, "camera refused the chosen mode; using its own");
    }
  }
  camera.open_stream().map_err(device)?;
  Ok(camera)
}

fn frame_format_of(format: PixelFormat) -> FrameFormat {
  match format {
    PixelFormat::Yuyv => FrameFormat::YUYV,
    PixelFormat::Mjpeg => FrameFormat::MJPEG,
    // `choose_mode` only returns the three formats above.
    _ => FrameFormat::NV12,
  }
}

fn pixel_format_of(format: FrameFormat) -> Option<PixelFormat> {
  match format {
    FrameFormat::MJPEG => Some(PixelFormat::Mjpeg),
    FrameFormat::YUYV => Some(PixelFormat::Yuyv),
    FrameFormat::NV12 => Some(PixelFormat::Nv12),
    _ => None,
  }
}
