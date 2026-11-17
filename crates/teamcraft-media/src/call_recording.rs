//! Records a call on this device (issue #216, e2ee.md §8.3): what it plays (everybody it hears)
//! and its own voice as the others hear it, mixed to one 48 kHz mono Opus stream in an Ogg file.
//!
//! - The app owns the [`CallRecorder`] (a call that ends doesn't take the recording with it) and
//!   hands the call's loop a [`RecordingSink`], which passes on each 20 ms frame played
//!   ([`RecordingSink::push_playback`]) and each Opus frame sent ([`RecordingSink::push_own`]); a
//!   thread of its own decodes the own voice, mixes and encodes. The played frames are the clock:
//!   one frame out per frame played.
//! - The file grows on disk page by page, never in memory: about 14 MB an hour (32 kbit/s). An Ogg
//!   file cut off by a crash plays up to its last page. The queue to the thread is bounded; a
//!   stalled disk drops frames instead of growing memory.
//! - The recording stops by itself at its maximum length, or when writing fails (a full disk):
//!   [`RecordingSink::is_ended`] tells the call's loop, which tells the others it stopped.
//! - The file is the app's to place in a private folder and to delete; nothing here uploads.
//!   Nothing is logged about what was said or about the file, only that a recording started and
//!   ended and how long it was.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::Duration;

use thiserror::Error;
use tracing::{debug, info, warn};

use crate::audio_codec::{
  AudioChannels, AudioContent, OpusDecoder, OpusEncoder, OpusEncoderConfig, OpusError,
};
use crate::audio_frame::{FRAME_SAMPLES_PER_CHANNEL, SAMPLE_RATE_HZ};
use crate::ogg_opus::OggOpusWriter;

/// Speech of several people at this rate stays clear in a mix; about 14 MB an hour.
const BITRATE_BPS: i32 = 32_000;
/// Frames queued for the recording thread (about ten seconds); beyond this, frames are dropped.
const MAX_QUEUED_FRAMES: usize = 512;
/// The own voice waiting for the next played frame, at most (200 ms): more means the two clocks
/// drifted, and the oldest goes.
const MAX_OWN_SAMPLES: usize = 10 * FRAME_SAMPLES_PER_CHANNEL;
/// The longest recording this recorder makes, whatever it is asked for (the policy's ceiling).
const MAX_LENGTH: Duration = Duration::from_secs(8 * 60 * 60);
/// How often the recording thread looks whether it should stop while nothing arrives.
const STOP_POLL: Duration = Duration::from_millis(100);

/// A recording under way. Dropping it stops it and deletes the file.
pub struct CallRecorder {
  sink: RecordingSink,
  maybe_thread: Option<JoinHandle<Result<u64, CallRecordingError>>>,
  path: PathBuf,
}

/// Where the call's loop hands its audio to a recording; any number of copies.
#[derive(Clone)]
pub struct RecordingSink {
  frames: SyncSender<Frame>,
  shared: Arc<Shared>,
}

/// A finished recording, for the app to post or delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallRecordingTake {
  pub path: PathBuf,
  pub duration_ms: u64,
  pub size_bytes: u64,
}

#[derive(Debug, Error)]
pub enum CallRecordingError {
  #[error("encoder: {0}")]
  Codec(#[from] OpusError),
  #[error("writing the recording: {0}")]
  File(#[from] std::io::Error),
  /// Stopped before anything was recorded.
  #[error("nothing was recorded")]
  Empty,
  #[error("the recording thread stopped")]
  Thread,
}

/// What the call's loop hands the recording thread.
enum Frame {
  /// 20 ms of what this device plays.
  Playback(Vec<f32>),
  /// One Opus frame this device sent: its own voice as the others hear it.
  Own(Vec<u8>),
}

#[derive(Default)]
struct Shared {
  is_stopping: AtomicBool,
  /// The thread is done: stopped, at the maximum, or writing failed.
  is_ended: AtomicBool,
  is_at_limit: AtomicBool,
  recorded_ms: AtomicU64,
}

/// The mix of the played frames and the own voice, frame by frame.
struct Mixer {
  own: VecDeque<f32>,
}

impl CallRecorder {
  /// Starts writing to `path` (created or replaced); `max` is the policy's longest recording.
  pub fn start(path: &Path, max: Duration) -> Result<Self, CallRecordingError> {
    let mut encoder = OpusEncoder::new(OpusEncoderConfig {
      content: AudioContent::Voice,
      bitrate_bps: BITRATE_BPS,
      // A file has no losses to plan for, and gaps of silence would break its timing.
      has_inband_fec: false,
      has_dtx: false,
      expected_loss_percent: 0,
      ..OpusEncoderConfig::default()
    })?;
    let decoder = OpusDecoder::new(AudioChannels::Mono)?;
    let pre_skip = encoder.lookahead_samples()?;
    let writer = OggOpusWriter::new(BufWriter::new(File::create(path)?), pre_skip)?;
    let (frames, frame_rx) = mpsc::sync_channel(MAX_QUEUED_FRAMES);
    let shared = Arc::new(Shared::default());
    let thread_shared = Arc::clone(&shared);
    let max_frames = (max.min(MAX_LENGTH).as_millis() / 20) as u64;
    let spawned = std::thread::Builder::new()
      .name("call-recording".to_owned())
      .spawn(move || {
        let result = record(&frame_rx, encoder, decoder, writer, max_frames, &thread_shared);
        thread_shared.is_ended.store(true, Ordering::Relaxed);
        result
      });
    let thread = match spawned {
      Ok(thread) => thread,
      Err(error) => {
        remove(path);
        return Err(CallRecordingError::File(error));
      },
    };
    info!(max_secs = max.as_secs(), "call recording started");
    Ok(Self {
      sink: RecordingSink { frames, shared },
      maybe_thread: Some(thread),
      path: path.to_path_buf(),
    })
  }

  /// What the call's loop hands its audio to.
  pub fn sink(&self) -> RecordingSink {
    self.sink.clone()
  }

  /// Whether the recording takes nothing more: it reached its maximum length, or writing failed
  /// ([`Self::finish`] says which).
  pub fn is_ended(&self) -> bool {
    self.sink.is_ended()
  }

  /// Whether it stopped at its maximum length.
  pub fn is_at_limit(&self) -> bool {
    self.sink.shared.is_at_limit.load(Ordering::Relaxed)
  }

  /// How much has been recorded.
  pub fn elapsed(&self) -> Duration {
    Duration::from_millis(self.sink.shared.recorded_ms.load(Ordering::Relaxed))
  }

  /// Stops and finishes the file.
  pub fn finish(mut self) -> Result<CallRecordingTake, CallRecordingError> {
    let result = self.join();
    let path = std::mem::take(&mut self.path);
    let samples = match result {
      Ok(samples) if samples > 0 => samples,
      Ok(_) => {
        remove(&path);
        return Err(CallRecordingError::Empty);
      },
      Err(error) => {
        remove(&path);
        return Err(error);
      },
    };
    let size_bytes = fs::metadata(&path)?.len();
    let duration_ms = samples * 1000 / u64::from(SAMPLE_RATE_HZ);
    info!(duration_secs = duration_ms / 1000, "call recording finished");
    Ok(CallRecordingTake {
      path,
      duration_ms,
      size_bytes,
    })
  }

  /// Stops and deletes the file.
  pub fn cancel(mut self) {
    drop(self.join());
    remove(&std::mem::take(&mut self.path));
    info!("call recording discarded");
  }

  /// Ends the thread once it wrote what is queued, and finishes the file.
  fn join(&mut self) -> Result<u64, CallRecordingError> {
    self.sink.shared.is_stopping.store(true, Ordering::Relaxed);
    let thread = self.maybe_thread.take().ok_or(CallRecordingError::Thread)?;
    thread.join().map_err(|_| CallRecordingError::Thread)?
  }
}

impl Drop for CallRecorder {
  fn drop(&mut self) {
    if self.maybe_thread.is_some() {
      drop(self.join());
    }
    if !self.path.as_os_str().is_empty() {
      remove(&self.path);
    }
  }
}

impl RecordingSink {
  /// 20 ms (960 samples) of what this device plays.
  pub fn push_playback(&self, mix: &[f32]) {
    self.send(Frame::Playback(mix.to_vec()));
  }

  /// One Opus frame this device sent. A muted microphone sends none, so it isn't recorded.
  pub fn push_own(&self, opus: &[u8]) {
    self.send(Frame::Own(opus.to_vec()));
  }

  /// Whether the recording takes nothing more (its maximum, or a failed write): the call stops
  /// handing it audio and tells the others.
  pub fn is_ended(&self) -> bool {
    self.shared.is_ended.load(Ordering::Relaxed)
  }

  fn send(&self, frame: Frame) {
    match self.frames.try_send(frame) {
      Ok(()) => {},
      // A stalled disk: this frame is lost, memory stays bounded.
      Err(TrySendError::Full(_)) => debug!("call recording queue full, frame dropped"),
      // The recording ended (stopped, full, or a write failed); its `finish` says why.
      Err(TrySendError::Disconnected(_)) => {},
    }
  }
}

impl Mixer {
  fn new() -> Self {
    Self {
      own: VecDeque::with_capacity(MAX_OWN_SAMPLES),
    }
  }

  fn push_own(&mut self, samples: &[i16]) {
    self.own.extend(
      samples
        .iter()
        .map(|sample| f32::from(*sample) / f32::from(i16::MAX)),
    );
    let excess = self.own.len().saturating_sub(MAX_OWN_SAMPLES);
    self.own.drain(..excess);
  }

  /// The played frame with as much of the own voice as is there, as 16-bit samples.
  fn mix(&mut self, playback: &[f32]) -> Vec<i16> {
    playback
      .iter()
      .map(|sample| {
        let own = self.own.pop_front().unwrap_or(0.0);
        to_i16(sample + own)
      })
      .collect()
  }
}

/// Deletes recordings left in `folder` (by a crash, or an app killed while recording). The shells
/// call it at start for their private recording folder (e2ee.md §8.3: a recording a crash left is
/// not kept).
pub fn clear_recordings(folder: &Path) {
  let Ok(entries) = fs::read_dir(folder) else {
    return;
  };
  for entry in entries.flatten() {
    if entry.file_type().is_ok_and(|kind| kind.is_file()) {
      remove(&entry.path());
    }
  }
}

/// The recording thread: until the queue closes or the maximum is reached. Returns the samples
/// written.
fn record(
  frames: &Receiver<Frame>,
  mut encoder: OpusEncoder,
  mut decoder: OpusDecoder,
  mut writer: OggOpusWriter<BufWriter<File>>,
  max_frames: u64,
  shared: &Shared,
) -> Result<u64, CallRecordingError> {
  let mut mixer = Mixer::new();
  let mut written_frames = 0u64;
  loop {
    // Once stopping, what is queued is written and then the file ends; the app stops handing
    // frames over first, so the queue runs empty.
    let frame = if shared.is_stopping.load(Ordering::Relaxed) {
      match frames.try_recv() {
        Ok(frame) => frame,
        Err(_) => break,
      }
    } else {
      match frames.recv_timeout(STOP_POLL) {
        Ok(frame) => frame,
        Err(RecvTimeoutError::Timeout) => continue,
        Err(RecvTimeoutError::Disconnected) => break,
      }
    };
    match frame {
      Frame::Own(opus) => match decoder.decode(&opus) {
        Ok(samples) => mixer.push_own(&samples),
        Err(error) => debug!(%error, "own voice frame not decoded for the recording"),
      },
      Frame::Playback(playback) if playback.len() == FRAME_SAMPLES_PER_CHANNEL => {
        let pcm = mixer.mix(&playback);
        let packet = encoder.encode(&pcm)?;
        writer.push(packet, FRAME_SAMPLES_PER_CHANNEL as u32)?;
        written_frames += 1;
        shared.recorded_ms.store(written_frames * 20, Ordering::Relaxed);
        if written_frames >= max_frames {
          shared.is_at_limit.store(true, Ordering::Relaxed);
          break;
        }
      },
      Frame::Playback(playback) => warn!(
        samples = playback.len(),
        "played frame of an unexpected length skipped"
      ),
    }
  }
  let samples = writer.samples();
  writer.finish()?;
  Ok(samples)
}

fn to_i16(sample: f32) -> i16 {
  // Invariant: the clamp keeps the product within i16.
  (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
}

fn remove(path: &Path) {
  if let Err(error) = fs::remove_file(path)
    && error.kind() != std::io::ErrorKind::NotFound
  {
    warn!(%error, "deleting a call recording failed");
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn played_frames_and_the_own_voice_make_one_file_of_the_played_length() {
    let folder = folder("mix");
    let path = folder.join("take.ogg");
    let recorder = CallRecorder::start(&path, Duration::from_secs(60)).unwrap();
    let sink = recorder.sink();
    let mut voice = OpusEncoder::new(OpusEncoderConfig::default()).unwrap();
    for index in 0..50 {
      sink.push_playback(&tone(index, 440.0));
      let own: Vec<i16> = tone(index, 660.0).iter().map(|sample| to_i16(*sample)).collect();
      sink.push_own(&voice.encode(&own).unwrap());
    }
    let take = recorder.finish().unwrap();
    assert_eq!(take.duration_ms, 1000, "one frame out per frame played");
    assert!(take.size_bytes > 1000);
    assert!(take.path.exists());
    let bytes = fs::read(&take.path).unwrap();
    assert_eq!(&bytes[..4], b"OggS");
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn it_stops_at_its_maximum_length() {
    let folder = folder("full");
    let recorder = CallRecorder::start(&folder.join("take.ogg"), Duration::from_millis(200)).unwrap();
    for index in 0..30 {
      recorder.sink().push_playback(&tone(index, 440.0));
    }
    let take = loop {
      if recorder.is_ended() {
        assert!(recorder.is_at_limit());
        break recorder.finish().unwrap();
      }
      std::thread::yield_now();
    };
    assert_eq!(take.duration_ms, 200);
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn an_empty_or_cancelled_recording_leaves_no_file() {
    let folder = folder("empty");
    let path = folder.join("take.ogg");
    let recorder = CallRecorder::start(&path, Duration::from_secs(60)).unwrap();
    assert!(matches!(recorder.finish(), Err(CallRecordingError::Empty)));
    assert!(!path.exists());
    let recorder = CallRecorder::start(&path, Duration::from_secs(60)).unwrap();
    recorder.sink().push_playback(&tone(0, 440.0));
    recorder.cancel();
    assert!(!path.exists());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn the_own_voice_never_queues_more_than_its_bound() {
    let mut mixer = Mixer::new();
    mixer.push_own(&vec![1000; MAX_OWN_SAMPLES * 3]);
    assert_eq!(mixer.own.len(), MAX_OWN_SAMPLES);
    let mixed = mixer.mix(&[0.0; FRAME_SAMPLES_PER_CHANNEL]);
    assert_eq!(mixed.len(), FRAME_SAMPLES_PER_CHANNEL);
    assert!(mixed.iter().all(|sample| *sample > 0));
    // Loud parts clip instead of wrapping around.
    let mut loud = Mixer::new();
    loud.push_own(&[i16::MAX; 4]);
    assert_eq!(loud.mix(&[1.0; 4]), vec![i16::MAX; 4]);
  }

  fn tone(index: usize, frequency_hz: f32) -> Vec<f32> {
    (0..FRAME_SAMPLES_PER_CHANNEL)
      .map(|sample| {
        let at = (index * FRAME_SAMPLES_PER_CHANNEL + sample) as f32 / SAMPLE_RATE_HZ as f32;
        0.3 * (std::f32::consts::TAU * frequency_hz * at).sin()
      })
      .collect()
  }

  fn folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("teamcraft-call-recording-{name}-{}", std::process::id()));
    fs::create_dir_all(&folder).unwrap();
    folder
  }
}
