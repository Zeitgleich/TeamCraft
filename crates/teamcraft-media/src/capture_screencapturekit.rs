//! Screens and their sound on macOS through ScreenCaptureKit, via the `screencapturekit` crate
//! (a safe API; our crates have no `unsafe`). Behind the `screencapturekit` feature, macOS 14+.
//!
//! Compile-checked only (in CI): written against the crate's source, never run on a Mac yet.
//! The first capture asks for the Screen Recording permission; until it is granted the system
//! hands out no content and the capture reports [`CaptureError::Denied`].
//!
//! What to share is picked in the system's own picker (`SCContentSharingPicker`): a window, an
//! application or a display, never one of this app's windows.
//!
//! Video and sound are separate streams so a share without sound costs nothing extra. The sound
//! stream sets `excludesCurrentProcessAudio`, so the call's own playback is never captured. Frames
//! are copied out as packed BGRA through a `CGImage` (the crate's direct pixel access needs
//! `unsafe`), then converted to I420 like every other capture.

use std::sync::Mutex;
use std::time::Instant;

use screencapturekit::content_sharing_picker::{
  SCContentSharingPicker, SCContentSharingPickerConfiguration, SCContentSharingPickerMode, SCPickerOutcome,
};
use screencapturekit::prelude::*;
use screencapturekit::screenshot_manager::CGImageExt;
use tracing::{debug, warn};

use crate::audio_frame::SAMPLE_RATE_HZ;
use crate::capture_convert::{PixelFormat as RawPixelFormat, RawPicture};
use crate::system_audio::{MonoResampler, SystemAudioCapture, SystemAudioError, SystemAudioSender};
use crate::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture};

const DISPLAY_PREFIX: &str = "display:";
const AUDIO_CHANNELS: i32 = 2;

/// A display, chosen by a token the app stores like the portal's restore token on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplaySource {
  /// The main display.
  Main,
  /// A display by its index in ScreenCaptureKit's list.
  Index(usize),
}

/// A display, window or application the user picked.
pub struct ScreenCaptureKitScreen {
  filter: SCContentFilter,
  /// The picked content's size in pixels.
  width: u32,
  height: u32,
  format: CaptureFormat,
  maybe_stream: Option<SCStream>,
}

/// The computer's sound minus this app's, through ScreenCaptureKit.
#[derive(Default)]
pub struct ScreenCaptureKitAudio {
  maybe_stream: Option<SCStream>,
}

/// Converts and queues the frames of a screen stream.
struct ScreenOutput {
  frames: FrameSender,
  start: Instant,
}

/// Converts and queues the sound of an audio stream.
struct AudioOutput {
  samples: SystemAudioSender,
  resampler: Mutex<MonoResampler>,
}

impl DisplaySource {
  /// `display:N` from an earlier share; anything else is the main display.
  pub fn from_token(maybe_token: Option<&str>) -> Self {
    maybe_token
      .and_then(|token| token.strip_prefix(DISPLAY_PREFIX))
      .and_then(|index| index.parse().ok())
      .map_or(Self::Main, Self::Index)
  }

  pub fn to_token(self) -> Option<String> {
    match self {
      Self::Main => None,
      Self::Index(index) => Some(format!("{DISPLAY_PREFIX}{index}")),
    }
  }
}

impl ScreenCaptureKitScreen {
  /// Lets the user pick a window, an application or a display in the system's picker (blocks
  /// until they did; call it off the main thread). This app's own windows can't be picked.
  pub fn pick(format: CaptureFormat) -> Result<Self, CaptureError> {
    let mut config = SCContentSharingPickerConfiguration::new().map_err(device_error)?;
    config.set_allowed_picker_modes(&[
      SCContentSharingPickerMode::SingleWindow,
      SCContentSharingPickerMode::SingleApplication,
      SCContentSharingPickerMode::SingleDisplay,
    ]);
    let content = SCShareableContent::get().map_err(|_| CaptureError::Denied("screen"))?;
    let own_windows: Vec<u32> = own_windows(&content).iter().map(SCWindow::window_id).collect();
    config.set_excluded_window_ids(&own_windows);
    let (picked_tx, picked_rx) = std::sync::mpsc::channel();
    SCContentSharingPicker::show(&config, move |outcome| {
      // The share may have been abandoned meanwhile; then nobody waits.
      let _ = picked_tx.send(outcome);
    });
    match picked_rx.recv() {
      Ok(SCPickerOutcome::Picked(result)) => {
        let (width, height) = result.pixel_size();
        Ok(Self {
          filter: result.filter(),
          width,
          height,
          format,
          maybe_stream: None,
        })
      },
      // Cancelled in the picker: nothing is shared.
      Ok(SCPickerOutcome::Cancelled) => Err(CaptureError::Cancelled("screen")),
      Ok(SCPickerOutcome::Error(error)) => Err(CaptureError::Device(error)),
      Err(_) => Err(CaptureError::Device("the picker closed without an answer".into())),
    }
  }
}

impl VideoCapture for ScreenCaptureKitScreen {
  fn format(&self) -> CaptureFormat {
    self.format
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    self.stop();
    let (width, height) = fit(self.width, self.height, self.format);
    let config = SCStreamConfiguration::new()
      .with_width(width)
      .with_height(height)
      .with_pixel_format(PixelFormat::BGRA)
      .with_shows_cursor(true)
      .with_minimum_frame_interval(&CMTime::new(1, i32::from(self.format.framerate.max(1))));
    let mut stream = SCStream::new(&self.filter, &config).map_err(device_error)?;
    stream
      .add_output_handler(
        ScreenOutput {
          frames,
          start: Instant::now(),
        },
        SCStreamOutputType::Screen,
      )
      .map_err(device_error)?;
    stream.start_capture().map_err(device_error)?;
    self.maybe_stream = Some(stream);
    Ok(())
  }

  fn stop(&mut self) {
    if let Some(stream) = self.maybe_stream.take()
      && let Err(error) = stream.stop_capture()
    {
      debug!(%error, "screen stream not stopped cleanly");
    }
  }
}

impl Drop for ScreenCaptureKitScreen {
  fn drop(&mut self) {
    self.stop();
  }
}

impl ScreenCaptureKitAudio {
  pub fn new() -> Self {
    Self::default()
  }
}

impl SystemAudioCapture for ScreenCaptureKitAudio {
  fn start(&mut self, samples: SystemAudioSender) -> Result<(), SystemAudioError> {
    self.stop();
    let display = shareable_display(DisplaySource::Main).map_err(|error| match error {
      CaptureError::Denied(_) => SystemAudioError::Denied,
      other => SystemAudioError::Device(other.to_string()),
    })?;
    // The sound of every application on the display; the picture is tiny and rare because only
    // the sound is used.
    let filter = SCContentFilter::create()
      .with_display(&display)
      .with_excluding_windows(&[])
      .build()
      .map_err(audio_error)?;
    let config = SCStreamConfiguration::new()
      .with_width(2)
      .with_height(2)
      .with_minimum_frame_interval(&CMTime::new(1, 1))
      .with_captures_audio(true)
      .with_excludes_current_process_audio(true)
      .with_sample_rate(SAMPLE_RATE_HZ as i32)
      .with_channel_count(AUDIO_CHANNELS);
    let mut stream = SCStream::new(&filter, &config).map_err(audio_error)?;
    stream
      .add_output_handler(
        AudioOutput {
          samples,
          resampler: Mutex::new(MonoResampler::new(AUDIO_CHANNELS as usize, SAMPLE_RATE_HZ)),
        },
        SCStreamOutputType::Audio,
      )
      .map_err(audio_error)?;
    stream.start_capture().map_err(audio_error)?;
    self.maybe_stream = Some(stream);
    Ok(())
  }

  fn stop(&mut self) {
    if let Some(stream) = self.maybe_stream.take()
      && let Err(error) = stream.stop_capture()
    {
      debug!(%error, "audio stream not stopped cleanly");
    }
  }
}

impl Drop for ScreenCaptureKitAudio {
  fn drop(&mut self) {
    self.stop();
  }
}

impl SCStreamOutputTrait for ScreenOutput {
  fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
    if !matches!(of_type, SCStreamOutputType::Screen) {
      return;
    }
    // Status-only samples (an unchanged screen) carry no image.
    let Ok(image) = sample.cg_image() else {
      return;
    };
    let (width, height) = (image.width(), image.height());
    let bgra = match image.bgra_data() {
      Ok(bgra) => bgra,
      Err(error) => {
        debug!(%error, "screen frame not readable");
        return;
      },
    };
    let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
      return;
    };
    let picture = RawPicture {
      format: RawPixelFormat::Bgrx,
      width,
      height,
      // `bgra_data` is packed.
      stride: width as usize * 4,
      data: &bgra,
    };
    let timestamp_us = u64::try_from(self.start.elapsed().as_micros()).unwrap_or(u64::MAX);
    match picture.to_i420(timestamp_us) {
      // A gone consumer stops the stream when the app stops the capture.
      Ok(frame) => {
        self.frames.send(frame);
      },
      Err(error) => debug!(%error, "captured frame dropped"),
    }
  }
}

impl SCStreamOutputTrait for AudioOutput {
  fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
    if !matches!(of_type, SCStreamOutputType::Audio) {
      return;
    }
    let Ok(buffers) = sample.audio_buffer_list() else {
      return;
    };
    let channels: Vec<Vec<f32>> = buffers.iter().map(|buffer| to_f32(buffer.data())).collect();
    let interleaved = interleave(&channels, buffers.iter().map(|buffer| buffer.number_channels));
    let mono = match self.resampler.lock() {
      Ok(mut resampler) => resampler.process(&interleaved),
      Err(_) => {
        warn!("system audio resampler poisoned");
        return;
      },
    };
    self.samples.send(mono);
  }
}

/// This app's windows (its process), so a share never shows the call itself.
fn own_windows(content: &SCShareableContent) -> Vec<SCWindow> {
  let Ok(own_pid) = i32::try_from(std::process::id()) else {
    return Vec::new();
  };
  content
    .windows()
    .into_iter()
    .filter(|window| {
      window
        .owning_application()
        .is_some_and(|application| application.process_id() == own_pid)
    })
    .collect()
}

/// The display to share; errors mean no permission or no such display.
fn shareable_display(source: DisplaySource) -> Result<SCDisplay, CaptureError> {
  // Without the Screen Recording permission there is no shareable content.
  let content = SCShareableContent::get().map_err(|_| CaptureError::Denied("screen"))?;
  let displays = content.displays();
  let index = match source {
    DisplaySource::Main => 0,
    DisplaySource::Index(index) => index,
  };
  displays
    .into_iter()
    .nth(index)
    .ok_or(CaptureError::NoDevice("display"))
}

/// The display's size scaled down to fit `format`, keeping its aspect ratio (even numbers, as
/// the encoder needs).
fn fit(display_width: u32, display_height: u32, format: CaptureFormat) -> (u32, u32) {
  let (max_width, max_height) = (
    u32::from(format.resolution.width),
    u32::from(format.resolution.height),
  );
  if display_width == 0 || display_height == 0 {
    return (max_width, max_height);
  }
  let scale = f64::min(
    1.0,
    f64::min(
      f64::from(max_width) / f64::from(display_width),
      f64::from(max_height) / f64::from(display_height),
    ),
  );
  let even = |value: f64| ((value.round() as u32) & !1).max(2);
  (
    even(f64::from(display_width) * scale),
    even(f64::from(display_height) * scale),
  )
}

/// 32-bit float samples from bytes (ScreenCaptureKit delivers float PCM).
fn to_f32(bytes: &[u8]) -> Vec<f32> {
  let (samples, _) = bytes.as_chunks::<4>();
  samples.iter().map(|sample| f32::from_ne_bytes(*sample)).collect()
}

/// One interleaved stereo signal from either one interleaved buffer or one buffer per channel.
fn interleave(buffers: &[Vec<f32>], channel_counts: impl Iterator<Item = u32>) -> Vec<f32> {
  let counts: Vec<u32> = channel_counts.collect();
  match (buffers, counts.as_slice()) {
    // Interleaved already, with the configured channel count.
    ([only], [count]) if *count as i32 == AUDIO_CHANNELS => only.clone(),
    // Mono: both sides.
    ([only], _) => only.iter().flat_map(|sample| [*sample, *sample]).collect(),
    // Planar: left and right buffers (extra buffers are ignored).
    ([left, right, ..], _) => left
      .iter()
      .zip(right)
      .flat_map(|(left, right)| [*left, *right])
      .collect(),
    ([], _) => Vec::new(),
  }
}

fn device_error(error: SCError) -> CaptureError {
  CaptureError::Device(error.to_string())
}

fn audio_error(error: SCError) -> SystemAudioError {
  SystemAudioError::Device(error.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::video::VideoResolution;

  const FULL_HD: CaptureFormat = CaptureFormat {
    resolution: VideoResolution {
      width: 1920,
      height: 1080,
    },
    framerate: 15,
  };

  #[test]
  fn tokens_round_trip() {
    assert_eq!(DisplaySource::from_token(None), DisplaySource::Main);
    assert_eq!(
      DisplaySource::from_token(Some("display:2")),
      DisplaySource::Index(2)
    );
    assert_eq!(DisplaySource::from_token(Some("monitor:2")), DisplaySource::Main);
    assert_eq!(DisplaySource::Index(1).to_token().as_deref(), Some("display:1"));
  }

  #[test]
  fn large_displays_shrink_to_fit_and_small_ones_stay() {
    assert_eq!(fit(5120, 2880, FULL_HD), (1920, 1080));
    assert_eq!(fit(3024, 1964, FULL_HD), (1662, 1080));
    assert_eq!(fit(1280, 800, FULL_HD), (1280, 800));
  }

  #[test]
  fn planar_and_mono_sound_become_interleaved_stereo() {
    let planar = interleave(&[vec![1.0, 2.0], vec![-1.0, -2.0]], [1, 1].into_iter());
    assert_eq!(planar, vec![1.0, -1.0, 2.0, -2.0]);
    let mono = interleave(&[vec![0.5]], [1].into_iter());
    assert_eq!(mono, vec![0.5, 0.5]);
    let interleaved = interleave(&[vec![0.1, 0.2]], [2].into_iter());
    assert_eq!(interleaved, vec![0.1, 0.2]);
  }
}
