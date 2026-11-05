//! Records a voice message (#204) on the desktop, Android and iOS: the microphone through the
//! call's audio input (cpal), encoded to Opus and written into an Ogg file as it goes, with the
//! level for the meter and the waveform the message carries.
//!
//! - The file grows on disk, never in memory: a take of the longest allowed length costs about
//!   a megabyte per four minutes (24 kbit/s), and the waveform keeps one level per 100 ms.
//! - The file is the app's to place in its private storage and to delete: the shells record into
//!   a folder only they read, and empty it at start (a take left by a crash is not sent).
//! - The recording stops by itself at the maximum length or when the microphone stops delivering;
//!   [`VoiceRecorder::maybe_stop`] says why, and the app shows the take for the user to send or
//!   discard. Nothing here sends anything.
//! - Nothing is logged about what was said; only that a recording started and ended, and errors.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use teamcraft_client_api::voice::{RecordingStop, Waveform};
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::audio_codec::{AudioContent, OpusEncoder, OpusEncoderConfig, OpusError};
use crate::audio_device::{AudioDeviceError, AudioInput, open_or_default};
use crate::audio_frame::{FRAME_SAMPLES_PER_CHANNEL, SAMPLE_RATE_HZ};
use crate::audio_io::AudioDeviceChoice;
use crate::audio_level::meter_level;
use crate::ogg_opus::OggOpusWriter;

/// Speech at this rate is clear in wideband Opus and keeps a ten-minute message under 2 MB.
const BITRATE_BPS: i32 = 24_000;
/// A waveform level is kept per this many 20 ms frames (100 ms).
const FRAMES_PER_LEVEL: usize = 5;
/// Without samples for this long the microphone is taken as gone.
const SILENT_DEVICE: Duration = Duration::from_secs(3);
/// How often the recording thread looks whether it should stop while nothing arrives.
const STOP_POLL: Duration = Duration::from_millis(100);
/// Capture buffers queued for the encoder; beyond this (a stalled disk) buffers are dropped.
const MAX_QUEUED_BUFFERS: usize = 512;
/// The longest take this recorder makes, whatever it is asked for.
const MAX_LENGTH: Duration = Duration::from_secs(60 * 60);

/// A recording under way. Dropping it cancels it (the file is deleted).
pub struct VoiceRecorder {
  shared: Arc<Shared>,
  maybe_thread: Option<JoinHandle<Result<Written, VoiceRecordingError>>>,
  path: PathBuf,
}

/// A finished take, ready to be sent as an attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceTake {
  pub path: PathBuf,
  pub duration_ms: u64,
  pub waveform: Vec<u8>,
  /// Why the recording stopped before the user finished it; `None` when they did.
  pub maybe_stop: Option<RecordingStop>,
}

#[derive(Debug, Error)]
pub enum VoiceRecordingError {
  /// The microphone didn't open: none, refused by the system (permission), or busy.
  #[error("microphone: {0}")]
  Microphone(#[from] AudioDeviceError),
  #[error("encoder: {0}")]
  Encoder(#[from] OpusError),
  #[error("writing the recording: {0}")]
  File(#[from] std::io::Error),
  /// Stopped before anything was recorded.
  #[error("nothing was recorded")]
  Empty,
  #[error("the recording thread stopped")]
  Thread,
}

/// What the recording thread and the app share.
#[derive(Default)]
struct Shared {
  is_stopping: AtomicBool,
  /// The meter's level (`f32` bits), from 0.0 to 1.0.
  level_bits: AtomicU32,
  elapsed_ms: AtomicU64,
  /// A [`RecordingStop`] the thread stopped for by itself, as [`stop_code`]; 0 while recording.
  stop: AtomicU8,
}

/// What the thread wrote.
struct Written {
  samples: u64,
  levels: Vec<f32>,
}

impl VoiceRecorder {
  /// Opens the microphone and starts writing to `path` (created or replaced). Returns once the
  /// microphone runs, or with the reason it doesn't (on the phones the app asks for the
  /// permission first). `max` is the policy's maximum length.
  pub fn start(path: &Path, choice: &AudioDeviceChoice, max: Duration) -> Result<Self, VoiceRecordingError> {
    let mut encoder = OpusEncoder::new(OpusEncoderConfig {
      content: AudioContent::Voice,
      bitrate_bps: BITRATE_BPS,
      // A file has no losses to plan for, and gaps of silence would break its timing.
      has_inband_fec: false,
      has_dtx: false,
      expected_loss_percent: 0,
      ..OpusEncoderConfig::default()
    })?;
    let pre_skip = encoder.lookahead_samples()?;
    let writer = OggOpusWriter::new(BufWriter::new(File::create(path)?), pre_skip)?;
    let shared = Arc::new(Shared::default());
    let (opened_tx, opened_rx) = mpsc::sync_channel(1);
    let thread_shared = Arc::clone(&shared);
    let choice = choice.clone();
    let max_frames = (max.min(MAX_LENGTH).as_millis() / 20) as u64;
    let spawned = std::thread::Builder::new()
      .name("voice-recording".to_owned())
      .spawn(move || record(&choice, encoder, writer, max_frames, &thread_shared, &opened_tx));
    let thread = match spawned {
      Ok(thread) => thread,
      Err(error) => {
        remove(path);
        return Err(VoiceRecordingError::File(error));
      },
    };
    let opened = opened_rx.recv().map_err(|_| VoiceRecordingError::Thread).and_then(|result| result);
    if let Err(error) = opened {
      drop(thread.join());
      remove(path);
      return Err(error);
    }
    info!("voice recording started");
    Ok(Self {
      shared,
      maybe_thread: Some(thread),
      path: path.to_path_buf(),
    })
  }

  /// The meter's level, from 0.0 (silence) to 1.0.
  pub fn level(&self) -> f32 {
    f32::from_bits(self.shared.level_bits.load(Ordering::Relaxed))
  }

  /// How much has been recorded.
  pub fn elapsed(&self) -> Duration {
    Duration::from_millis(self.shared.elapsed_ms.load(Ordering::Relaxed))
  }

  /// Why the recording stopped by itself, once it did: the app then shows the take.
  pub fn maybe_stop(&self) -> Option<RecordingStop> {
    stop_of(self.shared.stop.load(Ordering::Relaxed))
  }

  /// Stops and finishes the file. `maybe_reason` says why the app stopped it when the user
  /// didn't (a call, the app went to the background); the take keeps the first reason.
  pub fn finish(mut self, maybe_reason: Option<RecordingStop>) -> Result<VoiceTake, VoiceRecordingError> {
    if let Some(reason) = maybe_reason {
      drop(
        self
          .shared
          .stop
          .compare_exchange(0, stop_code(reason), Ordering::Relaxed, Ordering::Relaxed),
      );
    }
    let written = self.join()?;
    let maybe_stop = self.maybe_stop();
    let path = std::mem::take(&mut self.path);
    let Some(waveform) = Waveform::from_levels(&written.levels).filter(|_| written.samples > 0) else {
      remove(&path);
      return Err(VoiceRecordingError::Empty);
    };
    let duration_ms = written.samples * 1000 / u64::from(SAMPLE_RATE_HZ);
    info!(duration_ms, "voice recording finished");
    Ok(VoiceTake {
      path,
      duration_ms,
      waveform: waveform.into_bytes(),
      maybe_stop,
    })
  }

  /// Stops and deletes the file.
  pub fn cancel(mut self) {
    drop(self.join());
    remove(&std::mem::take(&mut self.path));
    info!("voice recording discarded");
  }

  fn join(&mut self) -> Result<Written, VoiceRecordingError> {
    self.shared.is_stopping.store(true, Ordering::Relaxed);
    let thread = self.maybe_thread.take().ok_or(VoiceRecordingError::Thread)?;
    thread.join().map_err(|_| VoiceRecordingError::Thread)?
  }
}

impl Drop for VoiceRecorder {
  fn drop(&mut self) {
    if self.maybe_thread.is_some() {
      drop(self.join());
    }
    if !self.path.as_os_str().is_empty() {
      remove(&self.path);
    }
  }
}

/// Deletes the takes left in `folder` (by a crash, or an app killed while recording). The shells
/// call it at start for their private recording folder.
pub fn clear_takes(folder: &Path) {
  let Ok(entries) = fs::read_dir(folder) else {
    return;
  };
  for entry in entries.flatten() {
    if entry.file_type().is_ok_and(|kind| kind.is_file()) {
      remove(&entry.path());
    }
  }
}

/// The recording thread: opens the microphone (it stays on this thread, cpal streams aren't
/// `Send`), then encodes 20 ms frames into the file until asked to stop or a limit is reached.
fn record(
  choice: &AudioDeviceChoice,
  mut encoder: OpusEncoder,
  mut writer: OggOpusWriter<BufWriter<File>>,
  max_frames: u64,
  shared: &Shared,
  opened: &mpsc::SyncSender<Result<(), VoiceRecordingError>>,
) -> Result<Written, VoiceRecordingError> {
  let (samples_tx, samples_rx) = mpsc::sync_channel::<Vec<f32>>(MAX_QUEUED_BUFFERS);
  let input = open_or_default(choice, |choice| {
    let samples_tx = samples_tx.clone();
    AudioInput::open(choice, move |samples| {
      // A full queue means the disk stalls; a dropped buffer is a short gap, not a stuck app.
      drop(samples_tx.try_send(samples.to_vec()));
    })
  });
  drop(samples_tx);
  let _input = match input {
    Ok(input) => {
      drop(opened.send(Ok(())));
      input
    },
    Err(error) => {
      drop(opened.send(Err(VoiceRecordingError::Microphone(error))));
      return Err(VoiceRecordingError::Thread);
    },
  };
  let mut frame: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES_PER_CHANNEL);
  let mut pcm = vec![0i16; FRAME_SAMPLES_PER_CHANNEL];
  let mut levels = Vec::new();
  let mut level_peak = 0.0f32;
  let mut frames: u64 = 0;
  let mut silent_for = Duration::ZERO;
  while !shared.is_stopping.load(Ordering::Relaxed) {
    let samples = match samples_rx.recv_timeout(STOP_POLL) {
      Ok(samples) => {
        silent_for = Duration::ZERO;
        samples
      },
      Err(RecvTimeoutError::Timeout) => {
        silent_for += STOP_POLL;
        if silent_for >= SILENT_DEVICE {
          warn!("the microphone stopped delivering; the voice recording stops");
          stop_by_itself(shared, RecordingStop::MicrophoneLost);
          break;
        }
        continue;
      },
      Err(RecvTimeoutError::Disconnected) => {
        stop_by_itself(shared, RecordingStop::MicrophoneLost);
        break;
      },
    };
    for chunk in samples.chunks(FRAME_SAMPLES_PER_CHANNEL) {
      let room = FRAME_SAMPLES_PER_CHANNEL - frame.len();
      let (now, rest) = chunk.split_at(room.min(chunk.len()));
      frame.extend_from_slice(now);
      if frame.len() < FRAME_SAMPLES_PER_CHANNEL {
        continue;
      }
      let level = meter_level(&frame);
      shared.level_bits.store(level.to_bits(), Ordering::Relaxed);
      level_peak = level_peak.max(rms(&frame));
      if (frames as usize + 1).is_multiple_of(FRAMES_PER_LEVEL) {
        levels.push(level_peak);
        level_peak = 0.0;
      }
      for (out, &sample) in pcm.iter_mut().zip(&frame) {
        *out = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
      }
      writer.push(encoder.encode(&pcm)?, FRAME_SAMPLES_PER_CHANNEL as u32)?;
      frames += 1;
      shared.elapsed_ms.store(frames * 20, Ordering::Relaxed);
      frame.clear();
      frame.extend_from_slice(rest);
      if frames >= max_frames {
        debug!("the voice recording reached its maximum length");
        stop_by_itself(shared, RecordingStop::LimitReached);
        break;
      }
    }
    if shared.stop.load(Ordering::Relaxed) == stop_code(RecordingStop::LimitReached) {
      break;
    }
  }
  if level_peak > 0.0 {
    levels.push(level_peak);
  }
  shared.level_bits.store(0.0f32.to_bits(), Ordering::Relaxed);
  let samples = writer.samples();
  writer.finish()?;
  Ok(Written { samples, levels })
}

fn stop_by_itself(shared: &Shared, reason: RecordingStop) {
  drop(
    shared
      .stop
      .compare_exchange(0, stop_code(reason), Ordering::Relaxed, Ordering::Relaxed),
  );
}

fn stop_code(reason: RecordingStop) -> u8 {
  match reason {
    RecordingStop::LimitReached => 1,
    RecordingStop::Call => 2,
    RecordingStop::Background => 3,
    RecordingStop::MicrophoneLost => 4,
  }
}

fn stop_of(code: u8) -> Option<RecordingStop> {
  match code {
    1 => Some(RecordingStop::LimitReached),
    2 => Some(RecordingStop::Call),
    3 => Some(RecordingStop::Background),
    4 => Some(RecordingStop::MicrophoneLost),
    _ => None,
  }
}

fn rms(samples: &[f32]) -> f32 {
  let sum: f32 = samples.iter().map(|sample| sample * sample).sum();
  (sum / samples.len().max(1) as f32).sqrt()
}

fn remove(path: &Path) {
  if let Err(error) = fs::remove_file(path)
    && error.kind() != std::io::ErrorKind::NotFound
  {
    warn!(%error, "a voice recording could not be deleted");
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn stop_reasons_round_trip() {
    for reason in [
      RecordingStop::LimitReached,
      RecordingStop::Call,
      RecordingStop::Background,
      RecordingStop::MicrophoneLost,
    ] {
      assert_eq!(stop_of(stop_code(reason)), Some(reason));
    }
    assert_eq!(stop_of(0), None);
  }

  #[test]
  fn leftover_takes_are_cleared() {
    let folder = std::env::temp_dir().join(format!("teamcraft-voice-test-{}", std::process::id()));
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("take.ogg"), b"half a take").unwrap();
    clear_takes(&folder);
    assert_eq!(fs::read_dir(&folder).unwrap().count(), 0);
    fs::remove_dir(&folder).unwrap();
  }
}
