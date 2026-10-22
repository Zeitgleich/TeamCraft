//! Screen and window capture on Windows through Windows Graphics Capture, via the
//! `windows-capture` crate (a safe API; our crates have no `unsafe`). Behind the `graphics-capture`
//! feature, Windows only.
//!
//! Compile-checked only: written against the crate's documentation and cross-checked with
//! `cargo xwin`, never run on a Windows machine yet. Windows 10 1903 or newer is required (the
//! API's minimum); the capture shows the system's yellow border on Windows 10.
//!
//! The source is chosen by a [`ScreenSource`], which round-trips through a string so the app can
//! store it like the portal's restore token on Linux. The system picker
//! (`GraphicsCapturePicker`) is not used: its item can't leave the thread that showed it, and a
//! capture that only ends on the next frame would keep the border up on a static screen.

use std::time::{Duration, Instant};

use thiserror::Error;
use tracing::warn;
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
  ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings, MinimumUpdateIntervalSettings,
  SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

use crate::capture_convert::{PixelFormat, RawPicture};
use crate::video::VideoResolution;
use crate::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture};

const MONITOR_PREFIX: &str = "monitor:";
const WINDOW_PREFIX: &str = "window:";

/// A screen or window that can be shared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenSource {
  /// The primary display.
  PrimaryMonitor,
  /// A display by its index in the system's enumeration.
  Monitor(usize),
  /// A top-level window by its exact title.
  Window(String),
}

/// One entry of the share picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenSourceInfo {
  pub source: ScreenSource,
  pub name: String,
}

/// A running or ready-to-run capture of one [`ScreenSource`].
pub struct WindowsScreenCapture {
  source: ScreenSource,
  format: CaptureFormat,
  maybe_running: Option<CaptureControl<FrameHandler, HandlerError>>,
}

#[derive(Debug, Error)]
#[error("{0}")]
struct HandlerError(String);

/// Receives frames on the capture thread and queues them for the encoder.
struct FrameHandler {
  frames: FrameSender,
  start: Instant,
  /// Row-padding-free copy of the last frame, reused to avoid an allocation per frame.
  scratch: Vec<u8>,
}

impl ScreenSource {
  /// The string stored as a setting (`monitor:1`, `window:Title`); the primary monitor is `None`.
  pub fn to_token(&self) -> Option<String> {
    match self {
      Self::PrimaryMonitor => None,
      Self::Monitor(index) => Some(format!("{MONITOR_PREFIX}{index}")),
      Self::Window(title) => Some(format!("{WINDOW_PREFIX}{title}")),
    }
  }

  /// Parses a stored token; anything unknown means the primary monitor.
  pub fn from_token(maybe_token: Option<&str>) -> Self {
    let Some(token) = maybe_token else {
      return Self::PrimaryMonitor;
    };
    if let Some(index) = token
      .strip_prefix(MONITOR_PREFIX)
      .and_then(|rest| rest.parse().ok())
    {
      Self::Monitor(index)
    } else if let Some(title) = token
      .strip_prefix(WINDOW_PREFIX)
      .filter(|title| !title.is_empty())
    {
      Self::Window(title.to_owned())
    } else {
      Self::PrimaryMonitor
    }
  }
}

/// Displays and visible, titled top-level windows, for a picker.
pub fn list_sources() -> Vec<ScreenSourceInfo> {
  let mut sources = Vec::new();
  if let Ok(monitors) = Monitor::enumerate() {
    for (index, monitor) in monitors.iter().enumerate() {
      let name = monitor
        .device_string()
        .unwrap_or_else(|_| format!("Display {}", index + 1));
      sources.push(ScreenSourceInfo {
        source: ScreenSource::Monitor(index),
        name,
      });
    }
  }
  if let Ok(windows) = Window::enumerate() {
    for window in windows.iter().filter(|window| window.is_valid()) {
      if let Ok(title) = window.title()
        && !title.trim().is_empty()
      {
        sources.push(ScreenSourceInfo {
          name: title.clone(),
          source: ScreenSource::Window(title),
        });
      }
    }
  }
  sources
}

impl WindowsScreenCapture {
  /// Prepares capturing `source`; nothing is captured before [`VideoCapture::start`]. `wanted`
  /// sets the frame rate and the fallback size; the real size is the source's.
  pub fn open(source: ScreenSource, wanted: CaptureFormat) -> Result<Self, CaptureError> {
    let resolution = source_resolution(&source).unwrap_or(wanted.resolution);
    Ok(Self {
      source,
      format: CaptureFormat {
        resolution,
        framerate: wanted.framerate,
      },
      maybe_running: None,
    })
  }

  pub fn source(&self) -> &ScreenSource {
    &self.source
  }
}

impl VideoCapture for WindowsScreenCapture {
  fn format(&self) -> CaptureFormat {
    self.format
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    self.stop();
    let flags = frames;
    let interval = Duration::from_secs(1) / u32::from(self.format.framerate.max(1));
    let control = match &self.source {
      ScreenSource::PrimaryMonitor => {
        let monitor = Monitor::primary().map_err(device_error)?;
        FrameHandler::start_free_threaded(settings(monitor, interval, flags))
      },
      ScreenSource::Monitor(index) => {
        let monitor = Monitor::from_index(index + 1).map_err(|_| CaptureError::NoDevice("display"))?;
        FrameHandler::start_free_threaded(settings(monitor, interval, flags))
      },
      ScreenSource::Window(title) => {
        let window = Window::from_name(title).map_err(|_| CaptureError::NoDevice("window"))?;
        FrameHandler::start_free_threaded(settings(window, interval, flags))
      },
    }
    .map_err(device_error)?;
    self.maybe_running = Some(control);
    Ok(())
  }

  fn stop(&mut self) {
    if let Some(control) = self.maybe_running.take()
      && let Err(error) = control.stop()
    {
      warn!(%error, "screen capture not stopped cleanly");
    }
  }
}

impl Drop for WindowsScreenCapture {
  fn drop(&mut self) {
    self.stop();
  }
}

impl GraphicsCaptureApiHandler for FrameHandler {
  type Flags = FrameSender;
  type Error = HandlerError;

  fn new(context: Context<Self::Flags>) -> Result<Self, Self::Error> {
    Ok(Self {
      frames: context.flags,
      start: Instant::now(),
      scratch: Vec::new(),
    })
  }

  fn on_frame_arrived(
    &mut self,
    frame: &mut Frame,
    capture_control: InternalCaptureControl,
  ) -> Result<(), Self::Error> {
    let buffer = frame.buffer().map_err(|error| HandlerError(error.to_string()))?;
    let (width, height) = (buffer.width(), buffer.height());
    let pixels = buffer.as_nopadding_buffer(&mut self.scratch);
    let picture = RawPicture {
      format: PixelFormat::Bgrx,
      width,
      height,
      stride: 0,
      data: pixels,
    };
    let timestamp_us = u64::try_from(self.start.elapsed().as_micros()).unwrap_or(u64::MAX);
    match picture.to_i420(timestamp_us) {
      Ok(frame) => {
        if !self.frames.send(frame) {
          capture_control.stop();
        }
      },
      Err(error) => warn!(%error, "screen frame dropped"),
    }
    Ok(())
  }
}

fn settings<T: TryInto<windows_capture::settings::GraphicsCaptureItemType>>(
  item: T,
  interval: Duration,
  frames: FrameSender,
) -> Settings<FrameSender, T> {
  Settings::new(
    item,
    CursorCaptureSettings::WithCursor,
    DrawBorderSettings::Default,
    SecondaryWindowSettings::Default,
    MinimumUpdateIntervalSettings::Custom(interval),
    DirtyRegionSettings::Default,
    ColorFormat::Bgra8,
    frames,
  )
}

/// The source's current size, rounded down to even numbers; `None` if it can't be read.
fn source_resolution(source: &ScreenSource) -> Option<VideoResolution> {
  let (width, height) = match source {
    ScreenSource::PrimaryMonitor => {
      let monitor = Monitor::primary().ok()?;
      (monitor.width().ok()?, monitor.height().ok()?)
    },
    ScreenSource::Monitor(index) => {
      let monitor = Monitor::from_index(index + 1).ok()?;
      (monitor.width().ok()?, monitor.height().ok()?)
    },
    ScreenSource::Window(title) => {
      let window = Window::from_name(title).ok()?;
      (
        u32::try_from(window.width().ok()?).ok()?,
        u32::try_from(window.height().ok()?).ok()?,
      )
    },
  };
  Some(VideoResolution {
    width: u16::try_from(width & !1).ok()?,
    height: u16::try_from(height & !1).ok()?,
  })
}

fn device_error(error: impl std::fmt::Display) -> CaptureError {
  CaptureError::Device(error.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tokens_round_trip() {
    for source in [
      ScreenSource::PrimaryMonitor,
      ScreenSource::Monitor(2),
      ScreenSource::Window("Notes: draft".into()),
    ] {
      assert_eq!(ScreenSource::from_token(source.to_token().as_deref()), source);
    }
  }

  #[test]
  fn unknown_tokens_mean_the_primary_monitor() {
    for token in ["", "monitor:x", "window:", "screen:1"] {
      assert_eq!(
        ScreenSource::from_token(Some(token)),
        ScreenSource::PrimaryMonitor
      );
    }
  }
}
