//! Records a call on this device (issue #216, e2ee.md §8.3): what it plays (everybody it hears)
//! and its own voice as the others hear it, mixed to one 48 kHz mono Opus stream in an Ogg file.
//!
//! - The app owns the [`CallRecorder`] (a call that ends doesn't take the recording with it) and
//!   hands the call's loop a [`RecordingSink`], which passes on each 20 ms frame played
//!   ([`RecordingSink::push_playback`]) and each Opus frame sent ([`RecordingSink::push_own`]); a
//!   thread of its own decodes the own voice, mixes and encodes. The played frames are the clock:
//!   one frame out per frame played.
//! - The file grows on disk chunk by chunk, never in memory: about 14 MB an hour (32 kbit/s). It is
//!   encrypted as it is written (`teamcraft-crypto::sealed_stream`, issue #2129) under a random key
//!   which goes with the [`CallRecordingTake`]; the take decrypts only while it is copied for the
//!   upload or to a file the user picked ([`CallRecordingTake::decrypt_to`]). The queue to the
//!   thread is bounded; a stalled disk drops frames instead of growing memory.
//! - **After a crash (#2116):** the app may hand [`CallRecorder::start`] a key of the device's local
//!   secrets ([`KeptKey`]); the recording's key is then kept next to the file, sealed under it with
//!   the app's note (`<name>.key`), until the recording is posted, saved and deleted, or discarded.
//!   At its next start the app lists the folder before it records anything ([`FolderAtStart`]) and
//!   gets back what a crash or a quit left ([`FolderAtStart::recover`]): a finished take as it
//!   was, an unfinished one up to its last chunk that verifies and its last whole Ogg page in it
//!   (about 16 seconds may be lost: the chunk that was still in memory). A file without a kept key,
//!   or whose key doesn't open, can't be read by anybody and is deleted.
//! - The recording stops by itself at its maximum length, or when writing fails (a full disk):
//!   [`RecordingSink::is_ended`] tells the call's loop, which tells the others it stopped.
//! - **With video (#2250):** [`CallRecorder::start_with_video`] adds a picture of the call
//!   (`call_recording_video`: the tiles the call screen shows, composited and encoded as H.264),
//!   and the file is a fragmented MP4 (`fmp4`) of the picture and the same Opus sound instead of
//!   Ogg. The sound is still the clock; pictures are placed by when they were composited. A crash
//!   keeps the file up to its last whole fragment (about two seconds) within the last whole chunk.
//! - The file is the app's to place in a private folder and to delete; nothing here uploads.
//!   Nothing is logged about what was said or about the file, only that a recording started and
//!   ended and how long it was.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use rand_core::OsRng;
use teamcraft_crypto::sealed_stream::{
  MAX_WRAP_NOTE_LEN, SealedStreamKey, SealedStreamReader, SealedStreamWriter,
};
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::DeviceId;
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::audio_codec::{
  AudioChannels, AudioContent, OpusDecoder, OpusEncoder, OpusEncoderConfig, OpusError,
};
use crate::audio_frame::{FRAME_SAMPLES_PER_CHANNEL, SAMPLE_RATE_HZ};
use crate::call_engine::RecordingAdmission;
use crate::call_recording_parts::{RECORDING_BITRATE_BPS, RecordingMixer, max_frames};
use crate::call_recording_video::{
  FIRST_PICTURE_WAIT, RecordedPicture, VideoRecording, VideoRecordingConfig, VideoRecordingSink, VideoShared,
};
use crate::fmp4::{
  FragmentedMp4Writer, Mp4Tracks, Mp4Video, VIDEO_TIMESCALE, copy_whole_fragments, parameter_sets,
};
use crate::ogg_opus::{OggOpusWriter, copy_whole_pages};

/// Frames queued for the recording thread (about ten seconds); beyond this, frames are dropped.
const MAX_QUEUED_FRAMES: usize = 512;
/// How often the recording thread looks whether it should stop while nothing arrives.
const STOP_POLL: Duration = Duration::from_millis(100);
/// The extension of a recording's kept key, next to it.
const KEY_EXTENSION: &str = "key";
/// The extension of a kept key being written.
const KEY_TEMPORARY_EXTENSION: &str = "key.tmp";
/// The extension of a recording of the sound only.
const OGG_EXTENSION: &str = "ogg";
/// The extension of a recording with video (#2250).
const MP4_EXTENSION: &str = "mp4";
/// The largest kept key read back: the wrapped key and the longest note.
const MAX_KEPT_KEY_BYTES: u64 = 1 + 24 + 32 + MAX_WRAP_NOTE_LEN as u64 + 16;

/// A recording under way. Dropping it stops it and deletes the file.
pub struct CallRecorder {
  sink: RecordingSink,
  maybe_thread: Option<JoinHandle<Result<Written, CallRecordingError>>>,
  /// The picture's thread, for a recording with video.
  maybe_video: Option<VideoRecording>,
  path: PathBuf,
  /// The file's key, for the take; only in memory, zeroized on drop.
  maybe_key: Option<SealedStreamKey>,
}

/// Where the call's loop hands its audio to a recording; any number of copies.
#[derive(Clone)]
pub struct RecordingSink {
  frames: SyncSender<Frame>,
  shared: Arc<Shared>,
}

/// A finished recording, for the app to post, save or delete. The file at `path` is encrypted;
/// only this value (and, when the app kept it, the sealed key next to the file) holds its key, so
/// delete the file with [`delete_recording`].
#[derive(Debug)]
pub struct CallRecordingTake {
  pub path: PathBuf,
  pub duration_ms: u64,
  /// The size of the file once decrypted: what the posted attachment shows.
  pub size_bytes: u64,
  pub format: RecordingFormat,
  key: SealedStreamKey,
  /// The file has its final chunk: the recording was finished, not cut by a crash.
  is_complete: bool,
}

/// What a recording's file is: its container, by its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingFormat {
  /// The sound only: Ogg Opus.
  OggOpus,
  /// The picture and the sound (#2250): H.264 and Opus in a fragmented MP4.
  Mp4,
}

/// Keeps a recording readable after a crash (#2116): its key is sealed under `device_key`, a key
/// of the device's local secrets, together with `note` (what the app needs to offer it again, at
/// most 1 KiB), in a file next to the recording.
pub struct KeptKey {
  pub device_key: SymmetricKey,
  pub note: Vec<u8>,
}

/// The files of a recording folder at the app's start, listed before anything new is recorded
/// there: what a crash or a quit left.
#[derive(Debug, Default)]
pub struct FolderAtStart {
  files: Vec<PathBuf>,
}

/// A recording a crash or a quit left, with its kept key and note.
#[derive(Debug)]
pub struct LeftoverRecording {
  pub take: CallRecordingTake,
  /// The note the app kept with the key.
  pub note: SecretBytes,
  /// When the file was last written: about when the recording ended.
  pub maybe_modified: Option<SystemTime>,
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

/// What the recording thread wrote.
struct Written {
  samples: u64,
  plaintext_bytes: u64,
}

/// What the call's loop hands the recording thread.
enum Frame {
  /// 20 ms of what this device plays.
  Playback(Vec<f32>),
  /// One Opus frame this device sent: its own voice as the others hear it.
  Own(Vec<u8>),
  /// One encoded picture of the call (#2250).
  Video(RecordedPicture),
}

#[derive(Default)]
struct Shared {
  is_stopping: AtomicBool,
  /// The thread is done: stopped, at the maximum, or writing failed.
  is_ended: AtomicBool,
  is_at_limit: AtomicBool,
  recorded_ms: AtomicU64,
  /// What the picture's thread shares, for a recording with video.
  maybe_video: Option<Arc<VideoShared>>,
}

impl CallRecorder {
  /// Starts writing to `path`, a new file; `max` is the policy's longest recording. With
  /// `maybe_kept`, the recording's key is kept next to it, sealed (see [`KeptKey`]); a key that
  /// can't be written (a full disk) is logged and the recording runs without it.
  pub fn start(path: &Path, max: Duration, maybe_kept: Option<KeptKey>) -> Result<Self, CallRecordingError> {
    Self::start_with_video(path, max, maybe_kept, None)
  }

  /// [`Self::start`], with a picture of the call where `maybe_video` gives its size and this
  /// device (#2250): `path` is then an MP4 file. Hand the pictures to [`Self::video_sink`].
  pub fn start_with_video(
    path: &Path,
    max: Duration,
    maybe_kept: Option<KeptKey>,
    maybe_video: Option<(VideoRecordingConfig, DeviceId)>,
  ) -> Result<Self, CallRecordingError> {
    let mut encoder = OpusEncoder::new(OpusEncoderConfig {
      content: AudioContent::Voice,
      bitrate_bps: RECORDING_BITRATE_BPS as i32,
      // A file has no losses to plan for, and gaps of silence would break its timing.
      has_inband_fec: false,
      has_dtx: false,
      expected_loss_percent: 0,
      ..OpusEncoderConfig::default()
    })?;
    let decoder = OpusDecoder::new(AudioChannels::Mono)?;
    let pre_skip = encoder.lookahead_samples()?;
    let key = SealedStreamKey::generate(&mut OsRng);
    if let Some(kept) = maybe_kept
      && let Err(error) = keep_key(path, &key, &kept)
    {
      warn!(%error, "the call recording's key could not be kept: a crash would lose it");
    }
    let file = match OpenOptions::new().write(true).create_new(true).open(path) {
      Ok(file) => file,
      Err(error) => {
        remove(&key_path(path));
        return Err(CallRecordingError::File(error));
      },
    };
    let opened = SealedStreamWriter::new(&key, file, &mut OsRng).and_then(|sealed| match &maybe_video {
      Some((config, _)) => Ok(Container::Mp4(Mp4State::Waiting {
        sealed: Box::new(sealed),
        pre_skip,
        config: *config,
        audio: Vec::new(),
      })),
      None => OggOpusWriter::new(sealed, pre_skip).map(Container::Ogg),
    });
    let writer = match opened {
      Ok(writer) => writer,
      Err(error) => {
        delete_recording(path);
        return Err(CallRecordingError::File(error));
      },
    };
    let (frames, frame_rx) = mpsc::sync_channel(MAX_QUEUED_FRAMES);
    let maybe_video_shared = maybe_video.as_ref().map(|_| Arc::new(VideoShared::default()));
    let shared = Arc::new(Shared {
      maybe_video: maybe_video_shared.clone(),
      ..Shared::default()
    });
    let thread_shared = Arc::clone(&shared);
    let max_frames = max_frames(max);
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
        delete_recording(path);
        return Err(CallRecordingError::File(error));
      },
    };
    let sink = RecordingSink { frames, shared };
    let maybe_video = match (maybe_video, maybe_video_shared) {
      (Some((config, own_device)), Some(video_shared)) => {
        let pictures = sink.frames.clone();
        let started = VideoRecording::start(config, own_device, video_shared, move |picture| {
          pictures.try_send(Frame::Video(picture)).is_ok()
        });
        match started {
          Ok(video) => Some(video),
          Err(error) => {
            // The writer waits for pictures that don't come, and then records the sound only.
            warn!(%error, "no thread for the call recording's video; it records the sound only");
            None
          },
        }
      },
      _ => None,
    };
    info!(
      max_secs = max.as_secs(),
      has_video = maybe_video.is_some(),
      "call recording started"
    );
    Ok(Self {
      sink,
      maybe_thread: Some(thread),
      maybe_video,
      path: path.to_path_buf(),
      maybe_key: Some(key),
    })
  }

  /// Where the call hands the recording its pictures, for a recording with video.
  pub fn video_sink(&self) -> Option<VideoRecordingSink> {
    self.maybe_video.as_ref().map(VideoRecording::sink)
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
    let written = match result {
      Ok(written) if written.samples > 0 => written,
      Ok(_) => {
        delete_recording(&path);
        return Err(CallRecordingError::Empty);
      },
      Err(error) => {
        delete_recording(&path);
        return Err(error);
      },
    };
    let Some(key) = self.maybe_key.take() else {
      delete_recording(&path);
      return Err(CallRecordingError::Thread);
    };
    let duration_ms = duration_ms_of(written.samples);
    info!(duration_secs = duration_ms / 1000, "call recording finished");
    let format = RecordingFormat::of(&path).unwrap_or(RecordingFormat::OggOpus);
    Ok(CallRecordingTake {
      path,
      duration_ms,
      size_bytes: written.plaintext_bytes,
      format,
      key,
      is_complete: true,
    })
  }

  /// Stops and deletes the file.
  pub fn cancel(mut self) {
    drop(self.join());
    delete_recording(&std::mem::take(&mut self.path));
    info!("call recording discarded");
  }

  /// Ends the thread once it wrote what is queued, and finishes the file.
  fn join(&mut self) -> Result<Written, CallRecordingError> {
    // The pictures stop first: what the writer then takes is the end of the file.
    if let Some(video) = self.maybe_video.as_mut() {
      video.stop();
    }
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
      delete_recording(&self.path);
    }
  }
}

impl CallRecordingTake {
  /// Writes the decrypted file to `output` (a new file in the core's upload staging folder, or
  /// where the user saves it), streaming, never the whole file in memory; returns the bytes
  /// written. A finished take that doesn't decrypt (the file was changed on disk) fails partway:
  /// the caller removes what `output` got. An unfinished one ([`Self::is_unfinished`]) gives what
  /// verifies, up to its last whole Ogg page, marked as the end. The take itself stays: the app
  /// deletes it once the copy is done, or keeps offering it.
  pub fn decrypt_to(&self, output: &mut impl Write) -> Result<u64, CallRecordingError> {
    let file = BufReader::new(File::open(&self.path)?);
    if self.is_complete {
      let mut reader = SealedStreamReader::new(&self.key, file)?;
      return Ok(io::copy(&mut reader, output)?);
    }
    let mut reader = SealedStreamReader::recovering(&self.key, file)?;
    let kept = self
      .format
      .copy_whole(&mut reader, output)
      .map_err(empty_if_invalid)?;
    Ok(kept.bytes)
  }

  /// A crash cut it: the file has no end, and decrypting keeps what was written before.
  pub fn is_unfinished(&self) -> bool {
    !self.is_complete
  }
}

impl RecordingFormat {
  /// The format of a recording's file, by its extension; `None` for any other file.
  pub fn of(path: &Path) -> Option<Self> {
    if has_extension(path, OGG_EXTENSION) {
      Some(Self::OggOpus)
    } else if has_extension(path, MP4_EXTENSION) {
      Some(Self::Mp4)
    } else {
      None
    }
  }

  /// The file name extension, without the dot.
  pub fn extension(self) -> &'static str {
    match self {
      Self::OggOpus => OGG_EXTENSION,
      Self::Mp4 => MP4_EXTENSION,
    }
  }

  /// The media type of the decrypted file, for the attachment.
  pub fn media_type(self) -> &'static str {
    match self {
      Self::OggOpus => "audio/ogg",
      Self::Mp4 => "video/mp4",
    }
  }

  /// Copies what is whole of a stream that may be cut short; the end marked where the container
  /// has one.
  fn copy_whole(self, input: &mut impl Read, output: &mut impl Write) -> io::Result<Kept> {
    match self {
      Self::OggOpus => copy_whole_pages(input, output).map(|kept| Kept {
        bytes: kept.bytes,
        samples: kept.samples,
      }),
      Self::Mp4 => copy_whole_fragments(input, output).map(|kept| Kept {
        bytes: kept.bytes,
        samples: kept.audio_samples,
      }),
    }
  }
}

/// What a copy of a recording cut short kept: its bytes, and its sound's samples (48 kHz).
struct Kept {
  bytes: u64,
  samples: u64,
}

impl FolderAtStart {
  /// The files in `folder` now (none when it doesn't exist).
  pub fn list(folder: &Path) -> Self {
    let files = fs::read_dir(folder)
      .map(|entries| {
        entries
          .flatten()
          .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
          .map(|entry| entry.path())
          .collect()
      })
      .unwrap_or_default();
    Self { files }
  }

  /// Whether a recording's key was kept: only then can anything be recovered, and only then does
  /// the app need its local secrets for it.
  pub fn has_kept_keys(&self) -> bool {
    self.files.iter().any(|file| has_extension(file, KEY_EXTENSION))
  }

  /// Deletes the files: nothing can read them (no key was kept, or the device key is gone).
  pub fn clear(self) {
    for file in &self.files {
      remove(file);
    }
  }

  /// The recordings that can be read with their kept key sealed under `device_key`, oldest first.
  /// Everything else is deleted: files without a key or whose key doesn't open, keys without a
  /// file, keys half written, recordings without any audio. A file that can't be read now (a
  /// disk error) stays for the next start. Reads every recording once (to know its length and
  /// whether it was finished), chunk by chunk: call it off the async runtime.
  pub fn recover(self, device_key: &SymmetricKey) -> Vec<LeftoverRecording> {
    let mut found = Vec::new();
    for file in &self.files {
      let Some(format) = RecordingFormat::of(file) else {
        // A kept key is read with its recording; anything else in the private folder goes.
        let is_kept_key = has_extension(file, KEY_EXTENSION)
          && [OGG_EXTENSION, MP4_EXTENSION]
            .iter()
            .any(|extension| file.with_extension(extension).exists());
        if !is_kept_key {
          remove(file);
        }
        continue;
      };
      match recover_one(file, format, device_key) {
        Ok(Some(leftover)) => found.push(leftover),
        Ok(None) => delete_recording(file),
        Err(error) => warn!(%error, "a call recording left at the last run could not be read now"),
      }
    }
    found.sort_by_key(|leftover| leftover.maybe_modified);
    if !found.is_empty() {
      info!(
        recordings = found.len(),
        "call recordings left at the last run were kept"
      );
    }
    found
  }
}

impl RecordingSink {
  /// 20 ms (960 samples) of what this device plays.
  pub fn push_playback(&self, mix: &[f32]) {
    self.send(Frame::Playback(mix.to_vec()));
  }

  /// Who the recording takes in (#2115), for its picture: a tile of anybody else is blanked. The
  /// call sets it with every played frame; it changes only when the answers do.
  pub fn set_admission(&self, admission: &RecordingAdmission<'_>) {
    if let Some(video) = &self.shared.maybe_video {
      video.set_admission(admission);
    }
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

/// Deletes a recording and its kept key, if there is one.
pub fn delete_recording(path: &Path) {
  remove(path);
  remove(&key_path(path));
}

/// Deletes every file in `folder`: recordings and their kept keys (the user signs out).
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
  mut writer: Container,
  max_frames: u64,
  shared: &Shared,
) -> Result<Written, CallRecordingError> {
  let mut mixer = RecordingMixer::new();
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
        Ok(samples) => mixer.push_own_i16(&samples),
        Err(error) => debug!(%error, "own voice frame not decoded for the recording"),
      },
      Frame::Video(picture) => {
        if let Some(video) = &shared.maybe_video {
          video.queued_pictures.fetch_sub(1, Ordering::Relaxed);
          writer.push_video(picture, video)?;
        }
      },
      Frame::Playback(playback) if playback.len() == FRAME_SAMPLES_PER_CHANNEL => {
        let pcm = mixer.mix_i16(&playback);
        let packet = encoder.encode(&pcm)?;
        writer.push_audio(packet, shared.maybe_video.as_deref())?;
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
  let (_, plaintext_bytes) = writer.finish()?.finish()?;
  Ok(Written {
    samples,
    plaintext_bytes,
  })
}

/// The file a recording writes into, sealed as it goes.
enum Container {
  Ogg(OggOpusWriter<SealedStreamWriter<File>>),
  Mp4(Mp4State),
}

/// An MP4 file waits for the first keyframe before its header (which carries the parameter sets
/// of the picture), at most [`FIRST_PICTURE_WAIT`]; the sound meanwhile waits in memory (a few
/// kilobytes). Without a keyframe by then it is written with the sound only.
enum Mp4State {
  Waiting {
    sealed: Box<SealedStreamWriter<File>>,
    pre_skip: u16,
    config: VideoRecordingConfig,
    /// The sound so far, and when its first frame came.
    audio: Vec<(Vec<u8>, Instant)>,
  },
  Writing {
    writer: Box<FragmentedMp4Writer<SealedStreamWriter<File>>>,
    /// When the first sound came: the file's zero, which pictures are placed from.
    started: Instant,
  },
  /// Only while switching between the two.
  Switching,
}

impl Container {
  fn push_audio(&mut self, packet: Vec<u8>, maybe_video: Option<&VideoShared>) -> io::Result<()> {
    let samples = FRAME_SAMPLES_PER_CHANNEL as u32;
    let state = match self {
      Self::Ogg(writer) => return writer.push(packet, samples),
      Self::Mp4(state) => state,
    };
    match state {
      Mp4State::Writing { writer, .. } => writer.push_audio(&packet, samples),
      Mp4State::Waiting { audio, .. } => {
        if audio.is_empty()
          && let Some(video) = maybe_video
        {
          // The sound starts: so do the pictures, with a keyframe for the header.
          video.is_keyframe_needed.store(true, Ordering::Relaxed);
          video.is_audio_started.store(true, Ordering::Relaxed);
        }
        let now = Instant::now();
        audio.push((packet, now));
        let started = audio[0].1;
        if now.duration_since(started) >= FIRST_PICTURE_WAIT {
          info!("no picture for the call recording in time; it records the sound only");
          state.open(None)?;
        }
        Ok(())
      },
      Mp4State::Switching => Ok(()),
    }
  }

  fn push_video(&mut self, picture: RecordedPicture, video: &VideoShared) -> io::Result<()> {
    let Self::Mp4(state) = self else {
      return Ok(());
    };
    if let Mp4State::Waiting { audio, .. } = state {
      let maybe_sets = picture
        .is_keyframe
        .then(|| parameter_sets(&picture.data))
        .flatten();
      match maybe_sets {
        Some(sets) if !audio.is_empty() => state.open(Some(sets))?,
        // Before the sound, or no keyframe yet: the file starts with one, after the sound.
        _ => {
          video.is_keyframe_needed.store(true, Ordering::Relaxed);
          return Ok(());
        },
      }
    }
    if let Mp4State::Writing { writer, started } = state {
      let Some(since) = picture.at.checked_duration_since(*started) else {
        return Ok(());
      };
      let time =
        u64::try_from(since.as_micros()).unwrap_or(u64::MAX) * u64::from(VIDEO_TIMESCALE) / 1_000_000;
      writer.push_video(&picture.data, time, picture.is_keyframe)?;
    }
    Ok(())
  }

  fn samples(&self) -> u64 {
    match self {
      Self::Ogg(writer) => writer.samples(),
      Self::Mp4(Mp4State::Writing { writer, .. }) => writer.audio_samples(),
      Self::Mp4(Mp4State::Waiting { audio, .. }) => audio.len() as u64 * FRAME_SAMPLES_PER_CHANNEL as u64,
      Self::Mp4(Mp4State::Switching) => 0,
    }
  }

  /// Ends the file; returns the sealed writer to finish.
  fn finish(self) -> io::Result<SealedStreamWriter<File>> {
    match self {
      Self::Ogg(writer) => writer.finish(),
      Self::Mp4(mut state) => {
        if matches!(state, Mp4State::Waiting { .. }) {
          state.open(None)?;
        }
        match state {
          Mp4State::Writing { writer, .. } => writer.finish().map(|(sealed, _)| sealed),
          Mp4State::Waiting { .. } | Mp4State::Switching => {
            Err(io::Error::other("the recording's file was not opened"))
          },
        }
      },
    }
  }
}

impl Mp4State {
  /// Writes the header (with the picture where `maybe_sets` has its parameter sets) and the sound
  /// that waited.
  fn open(&mut self, maybe_sets: Option<(Vec<u8>, Vec<u8>)>) -> io::Result<()> {
    let Self::Waiting {
      sealed,
      pre_skip,
      config,
      audio,
    } = std::mem::replace(self, Self::Switching)
    else {
      return Ok(());
    };
    let tracks = Mp4Tracks {
      maybe_video: maybe_sets.map(|(sps, pps)| Mp4Video {
        width: config.size.width,
        height: config.size.height,
        framerate: config.framerate,
        sps,
        pps,
      }),
      pre_skip,
    };
    let started = audio.first().map_or_else(Instant::now, |(_, at)| *at);
    let mut writer = FragmentedMp4Writer::new(*sealed, &tracks)?;
    for (packet, _) in &audio {
      writer.push_audio(packet, FRAME_SAMPLES_PER_CHANNEL as u32)?;
    }
    *self = Self::Writing {
      writer: Box::new(writer),
      started,
    };
    Ok(())
  }
}

/// Where a recording's key is kept.
fn key_path(recording: &Path) -> PathBuf {
  recording.with_extension(KEY_EXTENSION)
}

fn has_extension(path: &Path, extension: &str) -> bool {
  path
    .file_name()
    .and_then(|name| name.to_str())
    .is_some_and(|name| name.ends_with(&format!(".{extension}")))
}

fn duration_ms_of(samples: u64) -> u64 {
  samples * 1000 / u64::from(SAMPLE_RATE_HZ)
}

/// Writes the recording's key, sealed with the note, next to it: to a temporary file that is
/// synced and then renamed, so a crash leaves the whole key or none.
fn keep_key(recording: &Path, key: &SealedStreamKey, kept: &KeptKey) -> io::Result<()> {
  let wrapped = key
    .wrap(&kept.device_key, &kept.note, &mut OsRng)
    .map_err(io::Error::other)?;
  let temporary = recording.with_extension(KEY_TEMPORARY_EXTENSION);
  let written = (|| {
    let mut file = OpenOptions::new()
      .write(true)
      .create(true)
      .truncate(true)
      .open(&temporary)?;
    file.write_all(&wrapped)?;
    file.sync_all()?;
    fs::rename(&temporary, key_path(recording))
  })();
  if written.is_err() {
    remove(&temporary);
  }
  written
}

/// One recording a crash or a quit left: `None` when it can't be read (no key, a key that doesn't
/// open, no audio), an error when the disk failed reading it.
fn recover_one(
  path: &Path,
  format: RecordingFormat,
  device_key: &SymmetricKey,
) -> io::Result<Option<LeftoverRecording>> {
  let mut wrapped = Vec::new();
  match File::open(key_path(path)) {
    Ok(file) => file.take(MAX_KEPT_KEY_BYTES + 1).read_to_end(&mut wrapped)?,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
  let Ok((key, note)) = SealedStreamKey::unwrap(device_key, &wrapped) else {
    return Ok(None);
  };
  let file = File::open(path)?;
  let maybe_modified = file.metadata().and_then(|metadata| metadata.modified()).ok();
  let mut reader = match SealedStreamReader::recovering(&key, BufReader::new(file)) {
    Ok(reader) => reader,
    // Not even the header: nothing was recorded.
    Err(error)
      if matches!(
        error.kind(),
        io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData
      ) =>
    {
      return Ok(None);
    },
    Err(error) => return Err(error),
  };
  let kept = match format.copy_whole(&mut reader, &mut io::sink()) {
    Ok(kept) if kept.samples > 0 => kept,
    Ok(_) => return Ok(None),
    Err(error) if error.kind() == io::ErrorKind::InvalidData => return Ok(None),
    Err(error) => return Err(error),
  };
  let is_complete = reader.is_complete();
  Ok(Some(LeftoverRecording {
    take: CallRecordingTake {
      path: path.to_path_buf(),
      duration_ms: duration_ms_of(kept.samples),
      size_bytes: kept.bytes,
      format,
      key,
      is_complete,
    },
    note,
    maybe_modified,
  }))
}

/// A stream with no audio left to recover is an empty recording.
fn empty_if_invalid(error: io::Error) -> CallRecordingError {
  if error.kind() == io::ErrorKind::InvalidData {
    CallRecordingError::Empty
  } else {
    CallRecordingError::File(error)
  }
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
    let recorder = CallRecorder::start(&path, Duration::from_secs(60), None).unwrap();
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
    let sealed = fs::read(&take.path).unwrap();
    assert!(
      !sealed.windows(4).any(|window| window == b"OggS"),
      "nothing of the Ogg file on disk"
    );
    let mut posted = Vec::new();
    assert_eq!(take.decrypt_to(&mut posted).unwrap(), take.size_bytes);
    assert_eq!(&posted[..4], b"OggS");
    assert_eq!(posted.len() as u64, take.size_bytes);
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn a_take_changed_on_disk_does_not_decrypt() {
    let folder = folder("tampered");
    let recorder = CallRecorder::start(&folder.join("take.ogg"), Duration::from_secs(60), None).unwrap();
    for index in 0..50 {
      recorder.sink().push_playback(&tone(index, 440.0));
    }
    let take = recorder.finish().unwrap();
    let mut sealed = fs::read(&take.path).unwrap();
    let middle = sealed.len() / 2;
    sealed[middle] ^= 1;
    fs::write(&take.path, &sealed).unwrap();
    assert!(take.decrypt_to(&mut Vec::new()).is_err());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn it_stops_at_its_maximum_length() {
    let folder = folder("full");
    let recorder = CallRecorder::start(&folder.join("take.ogg"), Duration::from_millis(200), None).unwrap();
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
    let recorder = CallRecorder::start(&path, Duration::from_secs(60), None).unwrap();
    assert!(matches!(recorder.finish(), Err(CallRecordingError::Empty)));
    assert!(!path.exists());
    let recorder = CallRecorder::start(&path, Duration::from_secs(60), None).unwrap();
    recorder.sink().push_playback(&tone(0, 440.0));
    recorder.cancel();
    assert!(!path.exists());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn a_crash_leaves_a_recording_that_is_recovered_up_to_its_last_whole_chunk() {
    let folder = folder("crash");
    let device_key = SymmetricKey::generate(&mut OsRng);
    let path = folder.join("recording-1.ogg");
    let written = crashed_recording(&path, &device_key, 2000);
    let found = FolderAtStart::list(&folder);
    assert!(found.has_kept_keys());
    let mut leftovers = found.recover(&device_key);
    assert_eq!(leftovers.len(), 1);
    let leftover = leftovers.remove(0);
    assert_eq!(leftover.note.expose_secret(), b"note");
    let take = leftover.take;
    assert!(take.is_unfinished());
    // Two whole chunks of 64 KiB reached the disk; the rest died with the process.
    assert!(
      take.duration_ms > 10_000 && take.duration_ms < 40_000,
      "{}",
      take.duration_ms
    );
    let mut recovered = Vec::new();
    assert_eq!(take.decrypt_to(&mut recovered).unwrap(), take.size_bytes);
    assert_eq!(recovered.len() as u64, take.size_bytes);
    assert!(recovered.len() < written && recovered.len() > 2 * 60_000);
    // A valid stream: its pages copy over unchanged, the last one already marked as the end.
    let mut again = Vec::new();
    let kept = copy_whole_pages(&mut recovered.as_slice(), &mut again).unwrap();
    assert_eq!(again, recovered);
    assert_eq!(duration_ms_of(kept.samples), take.duration_ms);
    delete_recording(&take.path);
    assert_eq!(fs::read_dir(&folder).unwrap().count(), 0);
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn a_finished_take_left_by_a_quit_is_recovered_whole() {
    let folder = folder("quit");
    let device_key = SymmetricKey::generate(&mut OsRng);
    let path = folder.join("recording-1.ogg");
    let kept = KeptKey {
      device_key: device_key.duplicate(),
      note: b"conversation".to_vec(),
    };
    let recorder = CallRecorder::start(&path, Duration::from_secs(60), Some(kept)).unwrap();
    for index in 0..50 {
      recorder.sink().push_playback(&tone(index, 440.0));
    }
    let take = recorder.finish().unwrap();
    let mut posted = Vec::new();
    take.decrypt_to(&mut posted).unwrap();
    // The app quits: the take's key leaves memory, the kept one stays.
    drop(take);
    let leftovers = FolderAtStart::list(&folder).recover(&device_key);
    assert_eq!(leftovers.len(), 1);
    let take = &leftovers[0].take;
    assert!(!take.is_unfinished());
    assert_eq!(take.duration_ms, 1000);
    assert_eq!(take.size_bytes, posted.len() as u64);
    let mut recovered = Vec::new();
    take.decrypt_to(&mut recovered).unwrap();
    assert_eq!(recovered, posted);
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn the_leftover_scan_deletes_what_it_cannot_read() {
    let folder = folder("scan");
    let device_key = SymmetricKey::generate(&mut OsRng);
    // A recording without a kept key, a key without a recording, a key half written, a stray
    // file, a recording whose key is sealed under another device key, one with only a header.
    fs::write(folder.join("recording-1.ogg"), b"sealed").unwrap();
    fs::write(folder.join("recording-2.key"), b"key").unwrap();
    fs::write(folder.join("recording-3.key.tmp"), b"key").unwrap();
    fs::write(folder.join("stray"), b"?").unwrap();
    crashed_recording(
      &folder.join("recording-4.ogg"),
      &SymmetricKey::generate(&mut OsRng),
      2000,
    );
    crashed_recording(&folder.join("recording-5.ogg"), &device_key, 10);
    assert!(FolderAtStart::list(&folder).recover(&device_key).is_empty());
    assert_eq!(fs::read_dir(&folder).unwrap().count(), 0);
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn without_kept_keys_the_folder_is_cleared() {
    let folder = folder("no-keys");
    fs::write(folder.join("recording-1.ogg"), b"sealed").unwrap();
    let found = FolderAtStart::list(&folder);
    assert!(!found.has_kept_keys());
    found.clear();
    assert_eq!(fs::read_dir(&folder).unwrap().count(), 0);
    assert!(!FolderAtStart::list(&folder.join("missing")).has_kept_keys());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn a_key_that_cannot_be_kept_does_not_stop_the_recording() {
    let folder = folder("key-blocked");
    let path = folder.join("recording-1.ogg");
    // Something in the way of the key file: as a full disk would fail its write.
    fs::create_dir_all(key_path(&path)).unwrap();
    let kept = KeptKey {
      device_key: SymmetricKey::generate(&mut OsRng),
      note: Vec::new(),
    };
    let recorder = CallRecorder::start(&path, Duration::from_secs(60), Some(kept)).unwrap();
    for index in 0..50 {
      recorder.sink().push_playback(&tone(index, 440.0));
    }
    let take = recorder.finish().unwrap();
    assert_eq!(take.duration_ms, 1000);
    assert!(!folder.join("recording-1.key.tmp").exists());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn a_new_recording_never_replaces_a_file_that_is_there() {
    let folder = folder("no-replace");
    let path = folder.join("recording-1.ogg");
    fs::write(&path, b"left over").unwrap();
    assert!(CallRecorder::start(&path, Duration::from_secs(60), None).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"left over");
    fs::remove_dir_all(folder).unwrap();
  }

  mod with_video {
    use super::*;

    const SPS: [u8; 8] = [0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16];
    const PPS: [u8; 4] = [0x68, 0xce, 0x3c, 0x80];

    #[test]
    fn sound_and_pictures_make_one_mp4_file() {
      let folder = folder("mp4");
      let path = folder.join("recording-1.mp4");
      let (written, _) = mp4_recording(&path, 6, true);
      assert!(
        matches!(written, Container::Mp4(Mp4State::Writing { .. })),
        "opened at the first keyframe"
      );
      written.finish().unwrap().finish().unwrap();
      let take = CallRecordingTake {
        path: path.clone(),
        duration_ms: 6000,
        size_bytes: 0,
        format: RecordingFormat::of(&path).unwrap(),
        key: test_key(),
        is_complete: true,
      };
      let mut file = Vec::new();
      take.decrypt_to(&mut file).unwrap();
      assert_eq!(&file[4..8], b"ftyp");
      assert!(file.windows(4).any(|window| window == b"avcC"));
      let kept = crate::fmp4::copy_whole_fragments(&mut file.as_slice(), &mut io::sink()).unwrap();
      assert_eq!(kept.audio_samples, 6 * 48_000);
      assert_eq!(take.format.media_type(), "video/mp4");
      fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn without_a_keyframe_in_time_it_records_the_sound_only() {
      let folder = folder("mp4-no-picture");
      let path = folder.join("recording-1.mp4");
      let (written, shared) = mp4_recording(&path, 1, false);
      assert!(shared.is_audio_started.load(Ordering::Relaxed));
      assert!(
        shared.is_keyframe_needed.load(Ordering::Relaxed),
        "a keyframe is asked for"
      );
      written.finish().unwrap().finish().unwrap();
      let take = CallRecordingTake {
        path: path.clone(),
        duration_ms: 1000,
        size_bytes: 0,
        format: RecordingFormat::Mp4,
        key: test_key(),
        is_complete: true,
      };
      let mut file = Vec::new();
      take.decrypt_to(&mut file).unwrap();
      assert!(!file.windows(4).any(|window| window == b"avcC"));
      assert!(file.windows(4).any(|window| window == b"dOps"));
      fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn a_crash_leaves_an_mp4_recording_that_is_recovered_up_to_its_last_whole_fragment() {
      let folder = folder("mp4-crash");
      let device_key = SymmetricKey::generate(&mut OsRng);
      let path = folder.join("recording-1.mp4");
      keep_key(
        &path,
        &test_key(),
        &KeptKey {
          device_key: device_key.duplicate(),
          note: b"note".to_vec(),
        },
      )
      .unwrap();
      // Large pictures, so that whole sealed chunks reach the disk; then the process dies.
      let (written, _) = mp4_recording_sized(&path, 20, true, 4000);
      drop(written);
      let mut leftovers = FolderAtStart::list(&folder).recover(&device_key);
      assert_eq!(leftovers.len(), 1);
      let take = leftovers.remove(0).take;
      assert!(take.is_unfinished());
      assert_eq!(take.format, RecordingFormat::Mp4);
      assert!(
        take.duration_ms >= 2000 && take.duration_ms < 18_000,
        "whole fragments of about two seconds: {}",
        take.duration_ms
      );
      let mut recovered = Vec::new();
      assert_eq!(take.decrypt_to(&mut recovered).unwrap(), take.size_bytes);
      let kept = crate::fmp4::copy_whole_fragments(&mut recovered.as_slice(), &mut io::sink()).unwrap();
      assert_eq!(duration_ms_of(kept.audio_samples), take.duration_ms);
      fs::remove_dir_all(folder).unwrap();
    }

    /// The key every test file here is sealed with (a fixed one: the tests read their files back).
    fn test_key() -> SealedStreamKey {
      SealedStreamKey::generate(&mut FixedRng)
    }

    /// Always the same bytes: the same key every time.
    struct FixedRng;

    impl rand_core::RngCore for FixedRng {
      fn next_u32(&mut self) -> u32 {
        7
      }

      fn next_u64(&mut self) -> u64 {
        7
      }

      fn fill_bytes(&mut self, bytes: &mut [u8]) {
        bytes.fill(7);
      }

      fn try_fill_bytes(&mut self, bytes: &mut [u8]) -> Result<(), rand_core::Error> {
        bytes.fill(7);
        Ok(())
      }
    }

    impl rand_core::CryptoRng for FixedRng {}

    fn mp4_recording(path: &Path, seconds: u64, has_pictures: bool) -> (Container, Arc<VideoShared>) {
      mp4_recording_sized(path, seconds, has_pictures, 40)
    }

    /// `seconds` of sound (20 ms packets) and, with `has_pictures`, pictures at 15 fps with a
    /// keyframe every two seconds, placed from the first sound; not finished.
    fn mp4_recording_sized(
      path: &Path,
      seconds: u64,
      has_pictures: bool,
      picture_bytes: usize,
    ) -> (Container, Arc<VideoShared>) {
      let sealed = SealedStreamWriter::new(&test_key(), File::create(path).unwrap(), &mut OsRng).unwrap();
      let mut container = Container::Mp4(Mp4State::Waiting {
        sealed: Box::new(sealed),
        pre_skip: 312,
        config: VideoRecordingConfig::default(),
        audio: Vec::new(),
      });
      let shared = Arc::new(VideoShared::default());
      container.push_audio(vec![0xfc, 0, 0], Some(&shared)).unwrap();
      let started = match &container {
        Container::Mp4(Mp4State::Waiting { audio, .. }) => audio[0].1,
        _ => unreachable!(),
      };
      let mut next_picture = 0u64;
      for packet in 1..seconds * 50 {
        let now = Duration::from_millis(packet * 20);
        while has_pictures && Duration::from_micros(next_picture * 66_667) <= now {
          let is_keyframe = next_picture.is_multiple_of(30);
          let picture = RecordedPicture {
            at: started + Duration::from_micros(next_picture * 66_667 + 1),
            is_keyframe,
            data: picture(is_keyframe, picture_bytes),
          };
          container.push_video(picture, &shared).unwrap();
          next_picture += 1;
        }
        container
          .push_audio(vec![0xfc, packet as u8, 0], Some(&shared))
          .unwrap();
      }
      (container, shared)
    }

    fn picture(is_keyframe: bool, bytes: usize) -> Vec<u8> {
      let mut data = Vec::new();
      if is_keyframe {
        for set in [&SPS[..], &PPS[..]] {
          data.extend_from_slice(&[0, 0, 0, 1]);
          data.extend_from_slice(set);
        }
      }
      data.extend_from_slice(&[0, 0, 1, if is_keyframe { 0x65 } else { 0x41 }]);
      data.extend((0..bytes).map(|index| (index % 200 + 1) as u8));
      data
    }
  }

  /// What a crash leaves: the kept key, and a sealed Ogg stream of `packets` 20 ms packets whose
  /// writers were dropped without finishing. Returns the plaintext length written to them.
  fn crashed_recording(path: &Path, device_key: &SymmetricKey, packets: usize) -> usize {
    let key = SealedStreamKey::generate(&mut OsRng);
    let kept = KeptKey {
      device_key: device_key.duplicate(),
      note: b"note".to_vec(),
    };
    keep_key(path, &key, &kept).unwrap();
    let sealed = SealedStreamWriter::new(&key, File::create(path).unwrap(), &mut OsRng).unwrap();
    let mut writer = OggOpusWriter::new(sealed, 312).unwrap();
    for index in 0..packets {
      writer.push(vec![(index % 251) as u8; 80], 960).unwrap();
    }
    // Pages of 50 packets (about 4 KiB each) plus the two header pages.
    packets * 80 + packets / 50 * 80
  }

  fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
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
    drop(fs::remove_dir_all(&folder));
    fs::create_dir_all(&folder).unwrap();
    folder
  }
}
