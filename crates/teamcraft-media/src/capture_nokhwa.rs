//! Cameras on macOS (AVFoundation) and Windows (Media Foundation), via `nokhwa` (a safe wrapper;
//! our crates have no `unsafe` for Objective-C or COM calls). Behind the `avfoundation` feature on
//! macOS and the `mediafoundation` feature on Windows.
//!
//! Which camera opens follows `camera_choice` (the user's choice, else the built-in camera ahead of
//! an iPhone, Desk View or virtual cameras, and one fallback); its mode follows `capture_negotiate`.
//! Two nokhwa 0.10 quirks on AVFoundation shape this (#819): opening in "any" mode picks the first
//! mode listed, which is the bottom of a frame rate range (640x480@15) that nokhwa then refuses to
//! set; and nokhwa's own mode list comes back empty, so the modes are read through its bindings.
//!
//! On macOS nokhwa's stream asks AVFoundation for frames in the camera's own format, which for an
//! MJPEG mode is JPEG, a format AVFoundation's video data output doesn't deliver (#914): such
//! modes stream through `converted`, which has AVFoundation decode them to packed YUV.
//!
//! The macOS app's Info.plist needs `NSCameraUsageDescription`, or macOS denies access silently.
//! Windows is compile-checked only.

#[cfg(target_os = "macos")]
mod converted;

use std::time::Instant;

use nokhwa::Camera;
use nokhwa::NokhwaError;
use nokhwa::utils::{
  ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution,
};
#[cfg(target_os = "macos")]
use tracing::info;
use tracing::warn;

#[cfg(target_os = "macos")]
use self::converted::ConvertedStream;
use crate::camera_choice::{CameraReport, OpenCause, open_error, open_first, open_order, rank_cameras};
use crate::camera_thread::{CameraReader, CameraThread};
use crate::capture_convert::{PixelFormat, RawPicture};
use crate::capture_negotiate::{CameraBackend, OfferedMode, StartMode, open_negotiated};
use crate::video::VideoFrame;
use crate::video_capture::{CameraInfo, CameraKind, CaptureError, CaptureFormat, FrameSender, VideoCapture};

/// Frame formats `RawPicture::to_i420` converts (on macOS MJPEG modes arrive as YUYV, `converted`).
const SUPPORTED_FORMATS: &[FrameFormat] = &[FrameFormat::NV12, FrameFormat::YUYV, FrameFormat::MJPEG];
/// Marks an id made from nokhwa's index, for a camera without a unique id.
const INDEX_ID_PREFIX: &str = "index:";

/// A camera opened through nokhwa (AVFoundation on macOS, Media Foundation on Windows).
pub struct NokhwaCamera {
  maybe_chosen: Option<String>,
  format: CaptureFormat,
  report: CameraReport,
  maybe_running: Option<CameraThread>,
}

/// An open nokhwa camera, read on its camera thread. nokhwa's read blocks until a frame comes, for
/// good when none does: [`CameraThread`] stops it with a bounded wait.
struct NokhwaReader {
  camera: Camera,
  /// The stream of a mode nokhwa's own stream can't deliver; read instead of `camera`.
  maybe_converted: Option<ConvertedStream>,
}

/// nokhwa as the [`CameraBackend`] of `capture_negotiate`.
struct Nokhwa;

/// A camera nokhwa opened, with what we know about it (its kind words its errors).
struct NokhwaDevice {
  camera: Camera,
  info: CameraInfo,
  maybe_converted: Option<ConvertedStream>,
}

/// Windows: nokhwa's stream delivers every mode chosen here, there is no converted stream.
#[cfg(not(target_os = "macos"))]
enum ConvertedStream {}

/// Cameras the system reports, in the order of preference (`camera_choice`).
pub fn list_cameras() -> Vec<CameraInfo> {
  let listed = nokhwa::query(ApiBackend::Auto).unwrap_or_else(|error| {
    warn!(%error, "cameras not listed");
    Vec::new()
  });
  let cameras = listed
    .into_iter()
    .map(|camera| {
      let name = camera.human_name();
      CameraInfo {
        id: camera_id(&camera),
        kind: CameraKind::classify(&name, camera.description()),
        name,
      }
    })
    .collect();
  rank_cameras(cameras)
}

impl NokhwaCamera {
  /// `maybe_id`: the [`CameraInfo::id`] the user chose; `None`, or a camera that is no longer
  /// listed, opens the default. The camera opens when started, with one fallback
  /// (`camera_choice::open_order`); [`NokhwaCamera::report`] then says which one it is.
  pub fn open(maybe_id: Option<&str>, wanted: CaptureFormat) -> Result<Self, CaptureError> {
    ensure_camera_permission()?;
    Ok(Self {
      maybe_chosen: maybe_id.map(str::to_owned),
      format: wanted,
      report: CameraReport::default(),
      maybe_running: None,
    })
  }

  /// Which camera the last start opened.
  pub fn report(&self) -> CameraReport {
    self.report.clone()
  }
}

impl VideoCapture for NokhwaCamera {
  fn format(&self) -> CaptureFormat {
    self.format
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    self.stop();
    let maybe_chosen = self.maybe_chosen.clone();
    let wanted = self.format;
    let report = self.report.clone();
    // nokhwa's camera isn't `Send` on every platform: it is opened on the camera thread.
    let open = move || {
      let order = open_order(&list_cameras(), maybe_chosen.as_deref());
      let (device, used) = open_first(&order, |camera| open_negotiated(&mut Nokhwa, camera, wanted))?;
      report.set(used);
      Ok(NokhwaReader {
        camera: device.camera,
        maybe_converted: device.maybe_converted,
      })
    };
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

impl CameraBackend for Nokhwa {
  type Device = NokhwaDevice;

  fn open(&mut self, camera: &CameraInfo, start: StartMode) -> Result<NokhwaDevice, CaptureError> {
    let request = RequestedFormat::with_formats(request_type(start), SUPPORTED_FORMATS);
    let opened =
      Camera::new(camera_index(&camera.id), request).map_err(|error| capture_error(camera, &error))?;
    Ok(NokhwaDevice {
      camera: opened,
      info: camera.clone(),
      maybe_converted: None,
    })
  }

  fn modes(&mut self, device: &mut NokhwaDevice) -> Result<Vec<OfferedMode>, CaptureError> {
    let listed = listed_modes(device).map_err(|error| capture_error(&device.info, &error))?;
    Ok(offered_modes(&listed))
  }

  fn set_mode(&mut self, device: &mut NokhwaDevice, mode: OfferedMode) -> Result<(), CaptureError> {
    let exact = CameraFormat::new(
      Resolution::new(mode.width, mode.height),
      frame_format_of(mode.pixel_format),
      mode.framerate,
    );
    device
      .camera
      .set_camera_requset(RequestedFormat::with_formats(
        RequestedFormatType::Exact(exact),
        SUPPORTED_FORMATS,
      ))
      .map(drop)
      .map_err(|error| capture_error(&device.info, &error))
  }

  fn start_stream(&mut self, device: &mut NokhwaDevice) -> Result<(), CaptureError> {
    if let Some(converted) = converted_stream(device)? {
      device.maybe_converted = Some(converted);
      return Ok(());
    }
    device
      .camera
      .open_stream()
      .map_err(|error| capture_error(&device.info, &error))
  }
}

impl CameraReader for NokhwaReader {
  fn read(&mut self, start: Instant) -> Result<Option<VideoFrame>, CaptureError> {
    if let Some(converted) = &mut self.maybe_converted {
      return converted.read(start);
    }
    let buffer = self
      .camera
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
    Ok(frame_of(&picture, start))
  }

  fn close(&mut self) {
    if let Some(converted) = &mut self.maybe_converted {
      converted.close();
      return;
    }
    if let Err(error) = self.camera.stop_stream() {
      warn!(%error, "camera not stopped cleanly");
    }
  }
}

#[cfg(not(target_os = "macos"))]
impl ConvertedStream {
  fn read(&mut self, _start: Instant) -> Result<Option<VideoFrame>, CaptureError> {
    match *self {}
  }

  fn close(&mut self) {
    match *self {}
  }
}

/// A camera picture as I420, timed from `start`; `None` (logged) when it can't be converted.
fn frame_of(picture: &RawPicture, start: Instant) -> Option<VideoFrame> {
  let timestamp_us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
  match picture.to_i420(timestamp_us) {
    Ok(frame) => Some(frame),
    Err(error) => {
      warn!(%error, "camera frame dropped");
      None
    },
  }
}

/// macOS: the stream for a mode nokhwa's own stream can't deliver (#914); `None` for nokhwa's.
#[cfg(target_os = "macos")]
fn converted_stream(device: &mut NokhwaDevice) -> Result<Option<ConvertedStream>, CaptureError> {
  // The mode the camera is in now: the one set, or the one it opened in. Read back, because
  // nokhwa's bindings set a mode by its size and rate only, which can pick another format of the
  // same size.
  let mode = device
    .camera
    .refresh_camera_format()
    .map_err(|error| capture_error(&device.info, &error))?;
  if is_streamed_by_nokhwa(mode.format()) {
    return Ok(None);
  }
  info!(camera = %device.info.name, %mode, "AVFoundation converts the camera's frames to YUV");
  ConvertedStream::start(&camera_index(&device.info.id), mode)
    .map(Some)
    .map_err(|error| capture_error(&device.info, &error))
}

#[cfg(not(target_os = "macos"))]
fn converted_stream(_device: &mut NokhwaDevice) -> Result<Option<ConvertedStream>, CaptureError> {
  Ok(None)
}

/// Whether nokhwa's own stream delivers frames of a mode in `format`: AVFoundation's video data
/// output doesn't deliver JPEG, which nokhwa asks it for in an MJPEG mode.
#[cfg(target_os = "macos")]
fn is_streamed_by_nokhwa(format: FrameFormat) -> bool {
  format != FrameFormat::MJPEG
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

/// The camera's id for [`CameraInfo::id`]: AVFoundation's unique id (macOS) or the device's
/// symbolic link (Windows), both of which stay with the camera, where nokhwa's index changes with
/// every camera that comes or goes (and 1.0.8 stored the index, which nokhwa then looked up as a
/// unique id: "Device is null").
fn camera_id(camera: &nokhwa::utils::CameraInfo) -> String {
  let unique = camera.misc();
  if unique.is_empty() {
    format!("{INDEX_ID_PREFIX}{}", camera.index())
  } else {
    unique
  }
}

/// How nokhwa finds the camera of a [`camera_id`].
fn camera_index(id: &str) -> CameraIndex {
  match id
    .strip_prefix(INDEX_ID_PREFIX)
    .and_then(|index| index.parse().ok())
  {
    Some(index) => CameraIndex::Index(index),
    None => CameraIndex::String(id.to_owned()),
  }
}

fn request_type(start: StartMode) -> RequestedFormatType {
  match start {
    StartMode::HighestFrameRate => RequestedFormatType::AbsoluteHighestFrameRate,
    StartMode::HighestResolution => RequestedFormatType::AbsoluteHighestResolution,
    StartMode::FirstListed => RequestedFormatType::None,
  }
}

/// The modes as AVFoundation reports them. nokhwa's `compatible_camera_formats` comes back empty
/// on AVFoundation (its per-format list keeps the modes of the *other* formats), so the camera is
/// asked through nokhwa's bindings, the list nokhwa itself picks from when opening.
#[cfg(target_os = "macos")]
fn listed_modes(device: &mut NokhwaDevice) -> Result<Vec<CameraFormat>, NokhwaError> {
  nokhwa_bindings_macos::AVCaptureDevice::new(&camera_index(&device.info.id))?.supported_formats()
}

#[cfg(not(target_os = "macos"))]
fn listed_modes(device: &mut NokhwaDevice) -> Result<Vec<CameraFormat>, NokhwaError> {
  device.camera.compatible_camera_formats()
}

/// The modes we can capture, from what the platform lists (in our pixel formats, settable).
fn offered_modes(listed: &[CameraFormat]) -> Vec<OfferedMode> {
  let offered = listed
    .iter()
    .filter_map(|mode| {
      Some(OfferedMode {
        pixel_format: pixel_format_of(mode.format())?,
        width: mode.width(),
        height: mode.height(),
        framerate: mode.frame_rate(),
      })
    })
    .collect();
  settable(offered)
}

/// AVFoundation sets a mode only at the top of a frame rate range (`capture_negotiate::range_tops`).
#[cfg(target_os = "macos")]
fn settable(offered: Vec<OfferedMode>) -> Vec<OfferedMode> {
  crate::capture_negotiate::range_tops(&offered)
}

#[cfg(not(target_os = "macos"))]
fn settable(offered: Vec<OfferedMode>) -> Vec<OfferedMode> {
  offered
}

/// A nokhwa error in the user's terms for this camera (`camera_choice::open_error`).
fn capture_error(camera: &CameraInfo, error: &NokhwaError) -> CaptureError {
  open_error(camera.kind, cause_of(error), error.to_string())
}

/// What nokhwa's error says about the cause, by the messages its AVFoundation and Media
/// Foundation backends use.
fn cause_of(error: &NokhwaError) -> OpenCause {
  match error {
    NokhwaError::InitializeError { error, .. } if error.contains("in use") => OpenCause::InUse,
    // Another app holds the camera's configuration.
    NokhwaError::SetPropertyError { property, .. } if property == "lockForConfiguration" => OpenCause::InUse,
    NokhwaError::SetPropertyError { property, .. } if property == "CameraFormat" => OpenCause::NoUsableMode,
    NokhwaError::OpenDeviceError(message, _) if message == "Cannot fulfill request" => {
      OpenCause::NoUsableMode
    },
    NokhwaError::GetPropertyError { property, .. } if property.starts_with("Compatible Camera Format") => {
      OpenCause::NoUsableMode
    },
    NokhwaError::OpenDeviceError(_, reason) if reason == "Not Found" || reason == "Device is null" => {
      OpenCause::Gone
    },
    _ => OpenCause::Other,
  }
}

fn frame_format_of(format: PixelFormat) -> FrameFormat {
  match format {
    PixelFormat::Yuyv => FrameFormat::YUYV,
    PixelFormat::Mjpeg => FrameFormat::MJPEG,
    // Only the three formats `pixel_format_of` names are offered.
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
  use super::*;
  use crate::capture_negotiate::choose_mode;
  use crate::video::VideoResolution;

  /// The desktop's camera target (720p30).
  const WANTED: CaptureFormat = CaptureFormat {
    resolution: VideoResolution {
      width: 1280,
      height: 720,
    },
    framerate: 30,
  };

  /// #914: a USB webcam that offers MJPEG only, as AVFoundation lists it (`dmb1` formats, each
  /// with a 5–30 fps range that nokhwa lists as both ends).
  #[test]
  fn an_mjpeg_only_webcam_streams_720p30_converted_to_yuv() {
    let listed = listing(
      FrameFormat::MJPEG,
      &[(640, 480), (1280, 720), (1920, 1080)],
      &[5, 30],
    );
    let chosen = choose_mode(&offered_modes(&listed), WANTED).unwrap();
    assert_eq!(chosen, offered(PixelFormat::Mjpeg, 1280, 720, 30));
    assert!(
      !is_streamed_by_nokhwa(frame_format_of(chosen.pixel_format)),
      "nokhwa would ask for JPEG output, which AVFoundation doesn't deliver"
    );
  }

  /// The built-in camera's `420v` modes, which nokhwa's bindings call YUYV: nokhwa's own stream
  /// (`yuvs` output), as before.
  #[test]
  fn the_built_in_camera_keeps_nokhwa_s_stream() {
    let listed = listing(
      FrameFormat::YUYV,
      &[(640, 480), (1280, 720), (1920, 1080)],
      &[15, 30],
    );
    let chosen = choose_mode(&offered_modes(&listed), WANTED).unwrap();
    assert_eq!(chosen, offered(PixelFormat::Yuyv, 1280, 720, 30));
    assert!(is_streamed_by_nokhwa(frame_format_of(chosen.pixel_format)));
  }

  /// A webcam with both: raw YUV where USB has the bandwidth, MJPEG at full rate above it.
  #[test]
  fn a_webcam_with_yuv_and_mjpeg_modes_prefers_yuv_at_the_same_rate() {
    let mut listed = listing(FrameFormat::YUYV, &[(640, 480), (1280, 720)], &[30]);
    listed.extend(listing(FrameFormat::MJPEG, &[(1280, 720), (1920, 1080)], &[30]));
    let chosen = choose_mode(&offered_modes(&listed), WANTED).unwrap();
    assert_eq!(chosen, offered(PixelFormat::Yuyv, 1280, 720, 30));
    assert!(is_streamed_by_nokhwa(frame_format_of(chosen.pixel_format)));
  }

  #[test]
  fn a_webcam_whose_raw_720p_is_slow_takes_mjpeg_at_full_rate() {
    let mut listed = listing(FrameFormat::YUYV, &[(1280, 720)], &[10]);
    listed.extend(listing(FrameFormat::MJPEG, &[(1280, 720)], &[30]));
    let chosen = choose_mode(&offered_modes(&listed), WANTED).unwrap();
    assert_eq!(chosen, offered(PixelFormat::Mjpeg, 1280, 720, 30));
    assert!(!is_streamed_by_nokhwa(frame_format_of(chosen.pixel_format)));
  }

  #[test]
  fn formats_we_cannot_capture_are_not_offered() {
    let mut listed = listing(FrameFormat::GRAY, &[(1280, 720)], &[30]);
    listed.extend(listing(FrameFormat::RAWRGB, &[(1280, 720)], &[30]));
    assert!(offered_modes(&listed).is_empty());
  }

  /// Every size in `format` at every rate, as nokhwa lists a camera's formats.
  fn listing(format: FrameFormat, sizes: &[(u32, u32)], rates: &[u32]) -> Vec<CameraFormat> {
    sizes
      .iter()
      .flat_map(|&(width, height)| {
        rates
          .iter()
          .map(move |&rate| CameraFormat::new(Resolution::new(width, height), format, rate))
      })
      .collect()
  }

  fn offered(pixel_format: PixelFormat, width: u32, height: u32, framerate: u32) -> OfferedMode {
    OfferedMode {
      pixel_format,
      width,
      height,
      framerate,
    }
  }
}
