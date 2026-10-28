//! Screens and their sound on macOS through ScreenCaptureKit, via the `screencapturekit` crate
//! (a safe API; our crates have no `unsafe`). Behind the `screencapturekit` feature, macOS 14+.
//!
//! What to share is picked in the system's own picker (`SCContentSharingPicker`): a window, an
//! application or a display, never one of this app's windows. The picker itself grants the
//! capture of what was picked, so a share doesn't list the shareable content first (that asks for
//! the Screen Recording permission, which macOS applies only after the app restarted, issue
//! #683); this app's windows are left out by its bundle id instead. Only
//! [`ScreenCaptureKitScreen::display`] needs the permission. Run without an app bundle
//! (development), the own windows are listed only where the permission already applies, so the
//! picker never brings up the permission prompt either (#940).
//!
//! macOS may still refuse a picked capture when the stream starts (a privacy policy, or a grant
//! that no longer matches the app's signature, #940): that is [`CaptureError::Denied`], so the
//! user is told how to renew the permission.
//!
//! The capture passes on what macOS reports about its frames: frames it marks blank go on as
//! black pictures (the call notices a share that stays black and tells the user why), and a
//! stream the system stops (the user ended the share in the menu bar, the shared window closed,
//! the permission was taken back) ends the track instead of freezing it.
//!
//! A share's sound comes from the share's own stream, with the content filter of the pick (#914):
//! the sound of the picked display or application, or of the picked window's application. A
//! stream of its own for the whole display's sound would list the shareable content first, which
//! needs the Screen Recording permission (and an app restart after granting it). One stream for
//! picture and sound is also how Apple pairs the picker with sound; a share without sound
//! captures none. The stream sets `excludesCurrentProcessAudio`, so the call's own playback is
//! never captured.
//!
//! Frames are copied out as packed BGRA through a `CGImage` (the crate's direct pixel access needs
//! `unsafe`), then converted to I420 like every other capture.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use screencapturekit::SCFrameStatus;
use screencapturekit::content_sharing_picker::{
  SCContentSharingPicker, SCContentSharingPickerConfiguration, SCContentSharingPickerMode, SCPickerOutcome,
};
use screencapturekit::error::SCStreamErrorCode;
use screencapturekit::prelude::*;
use screencapturekit::screenshot_manager::CGImageExt;
use tracing::{debug, info, warn};

use crate::audio_frame::SAMPLE_RATE_HZ;
use crate::capture_convert::{PixelFormat as RawPixelFormat, RawPicture};
use crate::system_audio::{MonoResampler, SystemAudioCapture, SystemAudioError, SystemAudioSender};
use crate::video::{VideoFrame, VideoResolution};
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
  /// Set by [`Self::sound`]: the stream captures the picked content's sound too.
  maybe_sound: Option<SharedSound>,
}

/// The sound of what the user picked, minus this app's: captured by the share's own stream
/// ([`ScreenCaptureKitScreen::sound`]), so it needs no permission beyond the pick (#914).
pub struct ScreenCaptureKitAudio {
  sound: SharedSound,
}

/// Converts and queues the frames of a screen stream.
struct ScreenOutput {
  frames: SharedFrames,
  /// The size the stream was configured for: blank frames carry no picture of their own.
  size: (u32, u32),
  start: Instant,
  has_logged_blank: AtomicBool,
}

/// The frame queue a stream's output and its delegate share: the delegate drops it when the
/// system stops the stream, which ends the track.
type SharedFrames = Arc<Mutex<Option<FrameSender>>>;

/// Hears when the system stops a screen stream.
struct StreamEnd {
  frames: SharedFrames,
  maybe_sound: Option<SharedSound>,
}

/// Between a screen stream's audio output and the share's sound capture.
type SharedSound = Arc<Mutex<SoundTap>>;

/// Where a screen stream's sound goes.
#[derive(Debug, Default)]
struct SoundTap {
  /// The running stream has an audio output.
  is_captured: bool,
  /// The call's sound queue, while it takes the sound.
  maybe_samples: Option<SystemAudioSender>,
}

/// What a screen sample carries, by the status ScreenCaptureKit gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenSample {
  /// A new picture.
  Picture,
  /// Nothing changed since the last picture (no image; the encoder repeats the last one).
  Unchanged,
  /// macOS shows nothing of the content: before the Screen Recording permission applies, for
  /// protected content.
  Blank,
  /// The capture paused (the display slept, the screen locked) or ended.
  Paused,
}

/// Converts and queues the sound of a screen stream.
struct AudioOutput {
  sound: SharedSound,
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
  /// until they did; call it off the main thread). This app's own windows can't be picked:
  /// `maybe_own_bundle_id` leaves them out (the app's bundle id; `None` when not run as a bundle,
  /// in development, where they are left out by window if the system lists them).
  pub fn pick(format: CaptureFormat, maybe_own_bundle_id: Option<&str>) -> Result<Self, CaptureError> {
    let mut config = SCContentSharingPickerConfiguration::new().map_err(device_error)?;
    config.set_allowed_picker_modes(&[
      SCContentSharingPickerMode::SingleWindow,
      SCContentSharingPickerMode::SingleApplication,
      SCContentSharingPickerMode::SingleDisplay,
    ]);
    match maybe_own_bundle_id {
      Some(bundle_id) => config
        .set_excluded_bundle_ids(&[bundle_id])
        .map_err(|error| CaptureError::Device(error.to_string()))?,
      None => exclude_own_windows(&mut config),
    }
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
          maybe_sound: None,
        })
      },
      // Cancelled in the picker: nothing is shared.
      Ok(SCPickerOutcome::Cancelled) => Err(CaptureError::Cancelled("screen")),
      Ok(SCPickerOutcome::Error(error)) => Err(CaptureError::Device(error)),
      Err(_) => Err(CaptureError::Device("the picker closed without an answer".into())),
    }
  }

  /// Hides the picker of a share that was given up while it was open (on the main thread, where
  /// the picker lives). A later [`Self::pick`] answers the abandoned one with a cancel.
  pub fn close_picker() {
    SCContentSharingPicker::deactivate();
  }

  /// The picked content's sound, captured by this share's stream from its next start on (call it
  /// before [`VideoCapture::start`]). It delivers while the stream runs, and ends with it.
  pub fn sound(&mut self) -> ScreenCaptureKitAudio {
    let sound = self.maybe_sound.get_or_insert_with(SharedSound::default);
    ScreenCaptureKitAudio {
      sound: Arc::clone(sound),
    }
  }

  /// A whole display without the picker, for checks of the capture path
  /// (`examples/macos_capture.rs`). Needs the Screen Recording permission.
  pub fn display(source: DisplaySource, format: CaptureFormat) -> Result<Self, CaptureError> {
    let display = shareable_display(source)?;
    let filter = SCContentFilter::create()
      .with_display(&display)
      .with_excluding_windows(&[])
      .build()
      .map_err(device_error)?;
    Ok(Self {
      filter,
      width: display.width(),
      height: display.height(),
      format,
      maybe_stream: None,
      maybe_sound: None,
    })
  }
}

impl VideoCapture for ScreenCaptureKitScreen {
  fn format(&self) -> CaptureFormat {
    self.format
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    self.stop();
    let (width, height) = fit(self.width, self.height, self.format);
    let mut config = SCStreamConfiguration::new()
      .with_width(width)
      .with_height(height)
      .with_pixel_format(PixelFormat::BGRA)
      .with_shows_cursor(true)
      .with_minimum_frame_interval(&CMTime::new(1, i32::from(self.format.framerate.max(1))));
    if self.maybe_sound.is_some() {
      config = config
        .with_captures_audio(true)
        .with_excludes_current_process_audio(true)
        .with_sample_rate(SAMPLE_RATE_HZ as i32)
        .with_channel_count(AUDIO_CHANNELS);
    }
    let frames: SharedFrames = Arc::new(Mutex::new(Some(frames)));
    let end = StreamEnd {
      frames: Arc::clone(&frames),
      maybe_sound: self.maybe_sound.clone(),
    };
    let mut stream = SCStream::new_with_delegate(&self.filter, &config, end).map_err(device_error)?;
    stream
      .add_output_handler(
        ScreenOutput {
          frames,
          size: (width, height),
          start: Instant::now(),
          has_logged_blank: AtomicBool::new(false),
        },
        SCStreamOutputType::Screen,
      )
      .map_err(device_error)?;
    let has_sound_output = self
      .maybe_sound
      .as_ref()
      .is_some_and(|sound| add_sound_output(&mut stream, sound));
    stream.start_capture().map_err(start_error)?;
    if let Some(sound) = &self.maybe_sound {
      lock(sound).is_captured = has_sound_output;
    }
    self.maybe_stream = Some(stream);
    Ok(())
  }

  fn stop(&mut self) {
    if let Some(stream) = self.maybe_stream.take()
      && let Err(error) = stream.stop_capture()
    {
      debug!(%error, "screen stream not stopped cleanly");
    }
    // The sound ends with the stream that captures it.
    if let Some(sound) = &self.maybe_sound {
      lock(sound).end();
    }
  }
}

impl Drop for ScreenCaptureKitScreen {
  fn drop(&mut self) {
    self.stop();
  }
}

impl SystemAudioCapture for ScreenCaptureKitAudio {
  /// Fails when the share's stream captures no sound (it isn't running, or macOS refused its
  /// audio output); the share goes on without sound then.
  fn start(&mut self, samples: SystemAudioSender) -> Result<(), SystemAudioError> {
    lock(&self.sound).take(samples)
  }

  fn stop(&mut self) {
    lock(&self.sound).maybe_samples = None;
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
    let timestamp_us = u64::try_from(self.start.elapsed().as_micros()).unwrap_or(u64::MAX);
    let maybe_frame = match ScreenSample::of(sample.frame_status()) {
      ScreenSample::Picture => picture_of(&sample, timestamp_us),
      ScreenSample::Unchanged | ScreenSample::Paused => None,
      ScreenSample::Blank => {
        if !self.has_logged_blank.swap(true, Ordering::Relaxed) {
          warn!(
            "ScreenCaptureKit delivers blank frames: macOS shows nothing of the shared content \
             (before the Screen Recording permission applies, which needs an app restart, or \
             protected content)"
          );
        }
        // What macOS shows of it: black, so the call can tell the user.
        black_frame(self.size, timestamp_us)
      },
    };
    if let Some(frame) = maybe_frame {
      // A gone consumer stops the stream when the app stops the capture.
      let frames = lock(&self.frames);
      if let Some(frames) = frames.as_ref() {
        frames.send(frame);
      }
    }
  }
}

impl SCStreamDelegateTrait for StreamEnd {
  fn did_stop_with_error(&self, error: SCError) {
    warn!(%error, "the system stopped the screen capture; the share ends");
    // Dropping the queues ends the track and its sound: the encoder thread sees the capture end,
    // the call the sound.
    lock(&self.frames).take();
    if let Some(sound) = &self.maybe_sound {
      lock(sound).end();
    }
  }

  fn stream_did_become_inactive(&self) {
    info!("the screen capture became inactive");
  }
}

impl SoundTap {
  /// The call takes the sound from now on, if the stream captures any.
  fn take(&mut self, samples: SystemAudioSender) -> Result<(), SystemAudioError> {
    if !self.is_captured {
      return Err(SystemAudioError::Device(
        "the screen share captures no sound".into(),
      ));
    }
    self.maybe_samples = Some(samples);
    Ok(())
  }

  fn is_taken(&self) -> bool {
    self.maybe_samples.is_some()
  }

  /// Queues mono samples for the call; a call that is gone stops taking them.
  fn deliver(&mut self, mono: Vec<f32>) {
    if let Some(samples) = &self.maybe_samples
      && !samples.send(mono)
    {
      self.maybe_samples = None;
    }
  }

  /// The stream stopped: dropping the queue ends the call's sound.
  fn end(&mut self) {
    self.is_captured = false;
    self.maybe_samples = None;
  }
}

impl ScreenSample {
  /// A sample without a status is taken as a picture (it has one if it has an image).
  fn of(maybe_status: Option<SCFrameStatus>) -> Self {
    match maybe_status {
      None | Some(SCFrameStatus::Complete | SCFrameStatus::Started) => Self::Picture,
      Some(SCFrameStatus::Idle) => Self::Unchanged,
      Some(SCFrameStatus::Blank) => Self::Blank,
      Some(SCFrameStatus::Suspended | SCFrameStatus::Stopped | SCFrameStatus::Unknown(_)) => Self::Paused,
    }
  }
}

/// The sample's picture as I420; `None` when it has none or it can't be read.
fn picture_of(sample: &CMSampleBuffer, timestamp_us: u64) -> Option<VideoFrame> {
  let image = sample.cg_image().ok()?;
  let (width, height) = (image.width(), image.height());
  let bgra = match image.bgra_data() {
    Ok(bgra) => bgra,
    Err(error) => {
      debug!(%error, "screen frame not readable");
      return None;
    },
  };
  let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
    return None;
  };
  let picture = RawPicture {
    format: RawPixelFormat::Bgrx,
    width,
    height,
    // `bgra_data` is packed.
    stride: width as usize * 4,
    data: &bgra,
  };
  match picture.to_i420(timestamp_us) {
    Ok(frame) => Some(frame),
    Err(error) => {
      debug!(%error, "captured frame dropped");
      None
    },
  }
}

/// A black I420 picture of `size` (limited range: luma 16, grey chroma).
fn black_frame(size: (u32, u32), timestamp_us: u64) -> Option<VideoFrame> {
  let resolution = VideoResolution {
    width: u16::try_from(size.0).ok()?,
    height: u16::try_from(size.1).ok()?,
  };
  let pixels = usize::from(resolution.width) * usize::from(resolution.height);
  let mut data = vec![16; pixels];
  data.resize(pixels * 3 / 2, 128);
  VideoFrame::new(resolution, timestamp_us, data).ok()
}

impl SCStreamOutputTrait for AudioOutput {
  fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
    if !matches!(of_type, SCStreamOutputType::Audio) {
      return;
    }
    // Nothing to convert while the call doesn't take the sound (not yet, or not any more).
    if !lock(&self.sound).is_taken() {
      return;
    }
    let Ok(buffers) = sample.audio_buffer_list() else {
      return;
    };
    let channels: Vec<Vec<f32>> = buffers.iter().map(|buffer| to_f32(buffer.data())).collect();
    let interleaved = interleave(&channels, buffers.iter().map(|buffer| buffer.number_channels));
    let mono = lock(&self.resampler).process(&interleaved);
    lock(&self.sound).deliver(mono);
  }
}

/// Adds the stream's audio output for the share's sound; `false` if macOS refused it (the share
/// then goes on without sound, and the call shows its sound as unavailable).
fn add_sound_output(stream: &mut SCStream, sound: &SharedSound) -> bool {
  let output = AudioOutput {
    sound: Arc::clone(sound),
    resampler: Mutex::new(MonoResampler::new(AUDIO_CHANNELS as usize, SAMPLE_RATE_HZ)),
  };
  match stream.add_output_handler(output, SCStreamOutputType::Audio) {
    Ok(_) => true,
    Err(error) => {
      warn!(%error, "the share's sound can't be captured; the picture goes on without it");
      false
    },
  }
}

/// Whether the Screen Recording permission applies to this process now, asked without a prompt.
/// A share through the picker doesn't need it; it tells a refused capture apart for support
/// (granted yet refused: a grant that no longer matches this build, #940).
pub fn has_screen_recording_permission() -> bool {
  objc2_core_graphics::CGPreflightScreenCaptureAccess()
}

/// Development without an app bundle: leaves this process's windows out of the picker, if
/// ScreenCaptureKit lists them. Listing needs the Screen Recording permission and would ask for
/// it; the picker doesn't, so without the permission the windows stay pickable instead (#940).
fn exclude_own_windows(config: &mut SCContentSharingPickerConfiguration) {
  if !has_screen_recording_permission() {
    debug!("no Screen Recording permission: own windows not listed, they stay pickable");
    return;
  }
  match SCShareableContent::get() {
    Ok(content) => {
      let own_windows: Vec<u32> = own_windows(&content).iter().map(SCWindow::window_id).collect();
      config.set_excluded_window_ids(&own_windows);
    },
    Err(error) => debug!(%error, "own windows not listed; they stay pickable"),
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

/// A stream that didn't start: macOS refusing the capture (its TCC check, `SCStreamErrorUserDeclined`)
/// is [`CaptureError::Denied`], anything else a device error. The bridge hands a failed start over
/// as text only (Apple's English description, which ScreenCaptureKit doesn't translate).
fn start_error(error: SCError) -> CaptureError {
  let is_refused = match &error {
    SCError::PermissionDenied(_) => true,
    SCError::SCStreamError { code, .. } => *code == SCStreamErrorCode::UserDeclined,
    SCError::CaptureStartFailed(text) | SCError::StreamError(text) => is_refusal_text(text),
    _ => false,
  };
  if is_refused {
    warn!(%error, "macOS refused the screen capture");
    CaptureError::Denied("screen")
  } else {
    device_error(error)
  }
}

/// Whether a failed start's text is macOS refusing the capture ("The user declined TCCs for
/// application, window, display capture", code -3801).
fn is_refusal_text(text: &str) -> bool {
  let text = text.to_ascii_lowercase();
  text.contains("tcc") || text.contains("declined") || text.contains("-3801")
}

/// A lock that outlives a panic elsewhere: the queues stay usable.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
  use super::*;

  const FULL_HD: CaptureFormat = CaptureFormat {
    resolution: VideoResolution {
      width: 1920,
      height: 1080,
    },
    framerate: 15,
  };

  /// #940: a capture macOS refuses tells the user how to renew the permission.
  #[test]
  fn refused_starts_are_denied_and_other_failures_stay_device_errors() {
    for refused in [
      SCError::CaptureStartFailed(
        "Stream error: The user declined TCCs for application, window, display capture".into(),
      ),
      SCError::CaptureStartFailed("Stream error: The operation couldn't be completed. (-3801)".into()),
      SCError::from_stream_error_code(SCStreamErrorCode::UserDeclined),
      SCError::permission_denied("screen recording"),
    ] {
      assert!(
        matches!(start_error(refused.clone()), CaptureError::Denied("screen")),
        "{refused}"
      );
    }
    for other in [
      SCError::CaptureStartFailed("Stream error: Failed to start the stream".into()),
      SCError::from_stream_error_code(SCStreamErrorCode::NoCaptureSource),
      SCError::Timeout("start".into()),
    ] {
      assert!(
        matches!(start_error(other.clone()), CaptureError::Device(_)),
        "{other}"
      );
    }
  }

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
  fn frames_go_on_by_their_status() {
    assert_eq!(ScreenSample::of(None), ScreenSample::Picture);
    assert_eq!(
      ScreenSample::of(Some(SCFrameStatus::Complete)),
      ScreenSample::Picture
    );
    assert_eq!(
      ScreenSample::of(Some(SCFrameStatus::Started)),
      ScreenSample::Picture
    );
    assert_eq!(
      ScreenSample::of(Some(SCFrameStatus::Idle)),
      ScreenSample::Unchanged
    );
    assert_eq!(ScreenSample::of(Some(SCFrameStatus::Blank)), ScreenSample::Blank);
    for paused in [
      SCFrameStatus::Suspended,
      SCFrameStatus::Stopped,
      SCFrameStatus::Unknown(9),
    ] {
      assert_eq!(ScreenSample::of(Some(paused)), ScreenSample::Paused);
    }
  }

  #[test]
  fn blank_frames_become_black_pictures_of_the_stream_size() {
    let frame = black_frame((1662, 1080), 7).unwrap();
    assert_eq!(
      frame.resolution(),
      VideoResolution {
        width: 1662,
        height: 1080
      }
    );
    assert_eq!(frame.timestamp_us(), 7);
    assert_eq!(
      crate::picture_check::picture_content(&frame),
      crate::picture_check::PictureContent::Black
    );
    assert!(black_frame((70_000, 2), 0).is_none(), "too large for a frame");
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

  /// #914: the share's sound comes from the share's own stream, no stream of its own.
  mod sound {
    use tokio::sync::mpsc::error::TryRecvError;

    use super::*;
    use crate::system_audio::{SystemAudioReceiver, system_audio_queue};

    #[test]
    fn the_sound_starts_only_once_the_share_captures_it() {
      let sound = SharedSound::default();
      let mut capture = ScreenCaptureKitAudio {
        sound: Arc::clone(&sound),
      };
      let (samples, _receiver) = system_audio_queue();
      assert!(
        capture.start(samples.clone()).is_err(),
        "no running stream captures sound"
      );
      lock(&sound).is_captured = true;
      capture.start(samples).unwrap();
      assert!(lock(&sound).is_taken());
    }

    #[test]
    fn samples_reach_the_call_until_it_stops_taking_them() {
      let (sound, mut capture, mut receiver) = taken_sound();
      lock(&sound).deliver(vec![0.5; 480]);
      assert_eq!(receiver.try_recv().unwrap().len(), 480);
      capture.stop();
      lock(&sound).deliver(vec![0.5; 480]);
      assert!(matches!(receiver.try_recv(), Err(TryRecvError::Disconnected)));
      assert!(!lock(&sound).is_taken(), "nothing converted for nobody");
    }

    #[test]
    fn the_sound_ends_with_its_stream() {
      let (sound, mut capture, mut receiver) = taken_sound();
      // The share stopped, by the app or by the system.
      lock(&sound).end();
      assert!(matches!(receiver.try_recv(), Err(TryRecvError::Disconnected)));
      let (samples, _receiver) = system_audio_queue();
      assert!(capture.start(samples).is_err(), "no sound without its share");
    }

    #[test]
    fn a_call_that_is_gone_stops_taking_the_sound() {
      let (sound, _capture, receiver) = taken_sound();
      drop(receiver);
      lock(&sound).deliver(vec![0.5; 480]);
      assert!(!lock(&sound).is_taken());
    }

    fn taken_sound() -> (SharedSound, ScreenCaptureKitAudio, SystemAudioReceiver) {
      let sound = SharedSound::default();
      lock(&sound).is_captured = true;
      let mut capture = ScreenCaptureKitAudio {
        sound: Arc::clone(&sound),
      };
      let (samples, receiver) = system_audio_queue();
      capture.start(samples).unwrap();
      (sound, capture, receiver)
    }
  }
}
