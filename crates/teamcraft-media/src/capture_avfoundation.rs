//! Cameras on macOS through AVFoundation, via `nokhwa` (a safe wrapper; our crates have no
//! `unsafe` for Objective-C calls). Behind the `avfoundation` feature, macOS only.
//!
//! Untested: written and type-checked on Linux against nokhwa's V4L2 backend, never run on a
//! Mac. The app's Info.plist needs `NSCameraUsageDescription`, or macOS denies access silently.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use nokhwa::Camera;
use nokhwa::pixel_format::RgbAFormat;
use nokhwa::utils::{
  ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution,
};
use tracing::warn;

use crate::capture_convert::{PixelFormat, RawPicture};
use crate::video_capture::{CameraInfo, CaptureError, CaptureFormat, FrameSender, VideoCapture};

/// A camera opened through nokhwa (AVFoundation on macOS).
pub struct NokhwaCamera {
  index: CameraIndex,
  format: CaptureFormat,
  maybe_running: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
}

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
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let index = self.index.clone();
    let wanted = self.format;
    // nokhwa's camera isn't `Send` on every platform: open it on the capture thread.
    let (opened, opened_rx) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
      .name("camera".into())
      .spawn(move || {
        let requested = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Closest(CameraFormat::new(
          Resolution::new(
            u32::from(wanted.resolution.width),
            u32::from(wanted.resolution.height),
          ),
          FrameFormat::NV12,
          u32::from(wanted.framerate),
        )));
        let mut camera = match Camera::new(index, requested).and_then(|mut camera| {
          camera.open_stream()?;
          Ok(camera)
        }) {
          Ok(camera) => {
            let _ = opened.send(Ok(()));
            camera
          },
          Err(error) => {
            let _ = opened.send(Err(CaptureError::Device(error.to_string())));
            return;
          },
        };
        let start = Instant::now();
        while !thread_stop.load(Ordering::Relaxed) {
          let buffer = match camera.frame() {
            Ok(buffer) => buffer,
            Err(error) => {
              warn!(%error, "camera stopped");
              break;
            },
          };
          let Some(pixel_format) = pixel_format_of(buffer.source_frame_format()) else {
            warn!(format = ?buffer.source_frame_format(), "unsupported camera format");
            break;
          };
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
            Ok(frame) => {
              if !frames.send(frame) {
                break;
              }
            },
            Err(error) => warn!(%error, "camera frame dropped"),
          }
        }
        if let Err(error) = camera.stop_stream() {
          warn!(%error, "camera not stopped cleanly");
        }
      })
      .map_err(|error| CaptureError::Device(error.to_string()))?;
    let result = opened_rx
      .recv()
      .unwrap_or_else(|_| Err(CaptureError::Device("camera thread ended".into())));
    self.maybe_running = Some((stop, thread));
    if result.is_err() {
      self.stop();
    }
    result
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

impl Drop for NokhwaCamera {
  fn drop(&mut self) {
    self.stop();
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
