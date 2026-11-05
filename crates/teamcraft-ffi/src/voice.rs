//! Voice messages (#204) for the phones: recording through `teamcraft-media` (the microphone via
//! cpal, Opus in Ogg, written to the app's private folder), and decoding received Opus to WAV
//! where the platform's player reads neither Ogg nor WebM (iOS).
//!
//! The app asks for the microphone permission and sets up its audio session first; it stops the
//! recording on a call, backgrounding or an audio focus loss and shows the take for the user to
//! send or discard. A take to send is moved into the upload staging folder, which the core
//! empties once the upload is done; the recording folder itself is emptied at every start.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use teamcraft_client_api::voice::{self as voice_rules, RecordingStop, Waveform};
use teamcraft_media::audio_io::AudioDeviceChoice;
use teamcraft_media::voice_decode;
use teamcraft_media::voice_recording::{self, VoiceRecorder, VoiceRecordingError};

/// A recording under way. Every method may be called from any thread.
#[derive(uniffi::Object)]
pub struct VoiceRecording {
  maybe_recorder: Mutex<Option<VoiceRecorder>>,
}

/// Why a recording stopped before the user sent it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum VoiceStop {
  LimitReached,
  Call,
  Background,
  MicrophoneLost,
  /// The user left the conversation (the apps' own reason).
  Left,
}

/// A finished take.
#[derive(Clone, Debug, uniffi::Record)]
pub struct VoiceTakeInfo {
  pub path: String,
  pub duration_ms: u64,
  /// The waveform the message carries.
  pub waveform: Vec<u8>,
  pub maybe_stop: Option<VoiceStop>,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum VoiceError {
  /// The microphone didn't open (no permission, none there, busy): the app says how to allow it.
  #[error("microphone: {0}")]
  Microphone(String),
  /// Stopped before anything was recorded.
  #[error("nothing was recorded")]
  Empty,
  /// Not an Opus voice message, or a damaged one.
  #[error("not playable: {0}")]
  NotPlayable(String),
  /// Writing or reading the file failed, or the recorder stopped.
  #[error("failed: {0}")]
  Failed(String),
  /// The recording was already finished or discarded.
  #[error("the recording is over")]
  Over,
}

#[uniffi::export]
impl VoiceRecording {
  /// Starts recording into `path` (in the app's private recording folder), for at most
  /// `max_seconds` (the workspace policy's `max_seconds`).
  #[uniffi::constructor]
  pub fn start(path: String, max_seconds: u32) -> Result<Arc<Self>, VoiceError> {
    let recorder = VoiceRecorder::start(
      Path::new(&path),
      &AudioDeviceChoice::Default,
      Duration::from_secs(u64::from(max_seconds)),
    )?;
    Ok(Arc::new(Self {
      maybe_recorder: Mutex::new(Some(recorder)),
    }))
  }

  /// The meter's level, 0.0 to 1.0; 0.0 once over.
  pub fn level(&self) -> f32 {
    self.with(VoiceRecorder::level).unwrap_or(0.0)
  }

  pub fn elapsed_ms(&self) -> u64 {
    self
      .with(|recorder| recorder.elapsed().as_millis() as u64)
      .unwrap_or(0)
  }

  /// Why the recording stopped by itself (the maximum length, the microphone gone); the app
  /// then finishes it and shows the take.
  pub fn stopped_by_itself(&self) -> Option<VoiceStop> {
    self.with(VoiceRecorder::maybe_stop).flatten().map(VoiceStop::from)
  }

  /// Stops and finishes the file. `maybe_reason`: why the app stopped it, when the user didn't.
  pub fn finish(&self, maybe_reason: Option<VoiceStop>) -> Result<VoiceTakeInfo, VoiceError> {
    let recorder = self.take().ok_or(VoiceError::Over)?;
    let take = recorder.finish(maybe_reason.map(RecordingStop::from))?;
    Ok(VoiceTakeInfo {
      path: take.path.to_string_lossy().into_owned(),
      duration_ms: take.duration_ms,
      waveform: take.waveform,
      maybe_stop: take.maybe_stop.map(VoiceStop::from),
    })
  }

  /// Stops and deletes the file.
  pub fn cancel(&self) {
    if let Some(recorder) = self.take() {
      recorder.cancel();
    }
  }
}

impl VoiceRecording {
  fn with<T>(&self, read: impl FnOnce(&VoiceRecorder) -> T) -> Option<T> {
    self
      .maybe_recorder
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .as_ref()
      .map(read)
  }

  fn take(&self) -> Option<VoiceRecorder> {
    self
      .maybe_recorder
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .take()
  }
}

/// Empties the app's private recording folder: takes left by a crash or a killed app are never
/// sent. Call at start, before recording.
#[uniffi::export]
pub fn voice_clear_takes(folder: String) {
  voice_recording::clear_takes(Path::new(&folder));
}

/// Decodes a received Opus voice message (Ogg or WebM) at `input` to a 16-bit mono WAV file at
/// `output`, at most `max_seconds` long. Returns the decoded length in milliseconds.
#[uniffi::export]
pub fn voice_decode_to_wav(input: String, output: String, max_seconds: u32) -> Result<u64, VoiceError> {
  let length = voice_decode::decode_to_wav(
    Path::new(&input),
    Path::new(&output),
    Duration::from_secs(u64::from(max_seconds)),
  )
  .map_err(|error| match error {
    voice_decode::VoiceDecodeError::File(error) => VoiceError::Failed(error.to_string()),
    other => VoiceError::NotPlayable(other.to_string()),
  })?;
  Ok(length.as_millis() as u64)
}

/// `count` bar heights (0.0 to 1.0) of a received waveform for drawing; empty when the waveform
/// isn't one ([`Waveform::checked`]).
#[uniffi::export]
pub fn voice_waveform_heights(waveform: Vec<u8>, count: u32) -> Vec<f32> {
  Waveform::checked(&waveform)
    .map(|waveform| waveform.heights(count as usize))
    .unwrap_or_default()
}

/// The file name of a take recorded at that local time: `voice-message-20261105-140312.ogg`.
#[uniffi::export]
pub fn voice_file_name(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> String {
  voice_rules::file_name((year, month, day, hour, minute, second), "ogg")
}

impl From<VoiceRecordingError> for VoiceError {
  fn from(error: VoiceRecordingError) -> Self {
    match error {
      VoiceRecordingError::Microphone(error) => Self::Microphone(error.to_string()),
      VoiceRecordingError::Empty => Self::Empty,
      other => Self::Failed(other.to_string()),
    }
  }
}

impl From<RecordingStop> for VoiceStop {
  fn from(stop: RecordingStop) -> Self {
    match stop {
      RecordingStop::LimitReached => Self::LimitReached,
      RecordingStop::Call => Self::Call,
      RecordingStop::Background => Self::Background,
      RecordingStop::MicrophoneLost => Self::MicrophoneLost,
      RecordingStop::Left => Self::Left,
    }
  }
}

impl From<VoiceStop> for RecordingStop {
  fn from(stop: VoiceStop) -> Self {
    match stop {
      VoiceStop::LimitReached => Self::LimitReached,
      VoiceStop::Call => Self::Call,
      VoiceStop::Background => Self::Background,
      VoiceStop::MicrophoneLost => Self::MicrophoneLost,
      VoiceStop::Left => Self::Left,
    }
  }
}
