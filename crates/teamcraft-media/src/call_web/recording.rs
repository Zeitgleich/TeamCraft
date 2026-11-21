//! Records a call in the browser (issue #2114, e2ee.md §8.3), as the desktop app does
//! (`crate::call_recording`): what this page plays (everybody it hears) and its own voice, mixed
//! to one 48 kHz mono Opus stream in an Ogg file.
//!
//! - The call module owns the [`WebCallRecorder`] (a call that ends doesn't take the recording with
//!   it) and hands the call's loop a [`WebRecordingSink`], which passes on each 20 ms frame played
//!   ([`WebRecordingSink::push_playback`]) and each 20 ms of the own voice the call's encoder gets
//!   while the microphone is on ([`WebRecordingSink::push_own`]). The played frames are the clock:
//!   one frame out per frame played. WebCodecs encodes; the shared parts
//!   (`crate::call_recording_parts`) mix, and `crate::ogg_opus` writes the file.
//! - The file grows in the origin private file system (OPFS), piece by piece, never in memory
//!   whole: about 14 MB an hour. It is encrypted as it is written (`teamcraft-crypto::sealed_stream`,
//!   issue #2129) under a random key only this page holds, which goes with the
//!   [`WebRecordingTake`]; the take decrypts only while it is copied for the upload
//!   ([`WebRecordingTake::decrypt`]). A browser without a writable OPFS (Safari before 26, a
//!   private window) keeps the sealed file in memory instead, at most [`MEMORY_MAX_BYTES`] (about two
//!   hours); the recording stops there.
//! - Memory stays bounded however long the call: frames are dropped instead of queued when the
//!   encoder or the file falls behind ([`MAX_ENCODE_QUEUE`], [`MAX_IN_FLIGHT_BYTES`]).
//! - Each page writes into a folder of its own and holds a Web Lock named after it for as long as it
//!   lives. A page that was closed, reloaded or discarded while recording left a file nobody can
//!   read (its key died with the page); the next call module of the origin deletes such folders
//!   ([`RecordingFolder::sweep`]), never one whose page still runs.
//! - The recording stops by itself at its maximum length or when writing fails (the quota is
//!   used up): [`WebRecordingSink::is_ended`] tells the call's loop, which tells the others it
//!   stopped, and the module is called back to finish it.
//! - Nothing is logged about what was said or about the file, only that a recording started and
//!   ended and how long it was.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use js_sys::{Array, Float32Array, Object, Promise, Reflect, Uint8Array};
use rand_core::{OsRng, RngCore};
use teamcraft_crypto::sealed_stream::{CHUNK_SIZE, SealedStreamKey, SealedStreamReader, SealedStreamWriter};
use thiserror::Error;
use tracing::{debug, info, warn};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{Blob, BlobPropertyBag};

use super::js_message;
use crate::audio_frame::{FRAME_SAMPLES_PER_CHANNEL, SAMPLE_RATE_HZ};
use crate::call_recording_parts::{
  DEFAULT_PRE_SKIP, MAX_RECORDING_LENGTH, PrefetchedBytes, RECORDING_BITRATE_BPS, RecordingMixer,
  RecordingSpool, max_frames, pre_skip_of,
};
use crate::ogg_opus::OggOpusWriter;
use crate::web_api::{AudioData, AudioEncoder, EncodedAudioChunk, dictionary, has_global};

/// Pieces of this size go to the file (the sealed stream's chunk, about 16 seconds).
const WRITE_BYTES: usize = CHUNK_SIZE as usize;
/// Bytes handed to the file and not written yet, at most (about four minutes of audio); beyond,
/// frames are dropped instead of queued.
pub const MAX_IN_FLIGHT_BYTES: usize = 1024 * 1024;
/// Frames the encoder holds, at most (a second); beyond, frames are dropped.
pub const MAX_ENCODE_QUEUE: u32 = 50;
/// The sealed file kept in memory where the browser has no writable OPFS, at most.
pub const MEMORY_MAX_BYTES: u64 = 32 * 1024 * 1024;
/// The longest recording kept in memory: what fits in [`MEMORY_MAX_BYTES`] with room to spare.
pub const MEMORY_MAX_LENGTH: Duration = Duration::from_secs(2 * 60 * 60);
/// Where the pages of the origin keep their recordings, a folder each.
const FOLDER: &str = "teamcraft-call-recordings";
/// The Web Lock a page holds for as long as its folder may be in use.
const LOCK_PREFIX: &str = "teamcraft-call-recordings-";
/// Sealed bytes fetched ahead of the decryption: two chunks and their tags, with room to spare.
const READ_AHEAD: usize = 2 * (CHUNK_SIZE as usize + 1024);
/// The slice of the sealed file read at a time while decrypting.
const READ_SLICE: usize = 256 * 1024;
/// Plaintext collected before it is written to the copy for the upload.
const COPY_BYTES: usize = 256 * 1024;

/// This page's folder for recordings: in OPFS where the browser can write files there, else
/// none (recordings are then kept in memory).
pub struct RecordingFolder {
  page: String,
  maybe_opened: RefCell<Option<Opened>>,
  next: Cell<u64>,
}

#[derive(Clone)]
enum Opened {
  Opfs(Directory),
  Memory,
}

/// A recording under way. Dropping it stops it and deletes the file.
pub struct WebCallRecorder {
  sink: WebRecordingSink,
  maybe_key: Option<SealedStreamKey>,
  is_in_memory: bool,
  is_capped_by_memory: bool,
}

/// Where the call's loop hands its audio to a recording; any number of copies.
#[derive(Clone)]
pub struct WebRecordingSink(Rc<Shared>);

/// A finished recording, for the module to post or delete. Its file is encrypted; only this
/// value holds the key, so dropping it leaves the file unreadable (delete it too).
pub struct WebRecordingTake {
  file: SealedFile,
  key: SealedStreamKey,
  pub duration_ms: u64,
  /// The size of the Ogg file once decrypted: what the posted attachment shows.
  pub size_bytes: u64,
  /// It stopped at its maximum length (the policy's).
  pub is_at_limit: bool,
  /// It stopped at the most a browser without a writable OPFS keeps in memory.
  pub is_at_memory_limit: bool,
}

#[derive(Debug, Error)]
pub enum RecordingError {
  /// Stopped before anything was recorded.
  #[error("nothing was recorded")]
  Empty,
  #[error("the browser can't record calls: {0}")]
  Unsupported(&'static str),
  #[error("{context} failed: {message}")]
  Browser { context: &'static str, message: String },
  #[error("writing the recording: {0}")]
  File(String),
}

/// How a recording ended by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordingEnd {
  AtLimit,
  Failed(String),
}

/// The sink's state and the encoder; the encoder's callbacks hold [`Inner`] only.
struct Shared {
  inner: Rc<Inner>,
  encoder: AudioEncoder,
  _output: Closure<dyn FnMut(EncodedAudioChunk, JsValue)>,
  _error: Closure<dyn FnMut(JsValue)>,
}

struct Inner {
  state: RefCell<State>,
  /// Bytes sent to the file task and not written yet.
  in_flight: Cell<usize>,
  maybe_to_file: RefCell<Option<mpsc::UnboundedSender<ToFile>>>,
  /// Called once when the recording ends by itself.
  maybe_on_end: RefCell<Option<Box<dyn Fn()>>>,
}

struct State {
  mixer: RecordingMixer,
  writer: Writer,
  spool: RecordingSpool,
  timestamp_us: f64,
  /// Frames handed to the encoder: the file holds as many packets. The encoder's flush puts out
  /// one more (the rest of its lookahead), which is left out so the file is as long as what was
  /// played.
  frames_encoded: u64,
  frames_written: u64,
  max_frames: u64,
  maybe_memory_cap: Option<u64>,
  maybe_end: Option<RecordingEnd>,
  is_stopping: bool,
  dropped_frames: u64,
}

/// The sealed stream before the first packet (whose metadata tells the pre-skip), then the Ogg
/// stream in it.
enum Writer {
  Waiting(SealedStreamWriter<RecordingSpool>),
  Writing(OggOpusWriter<SealedStreamWriter<RecordingSpool>>),
  Done,
}

enum ToFile {
  Bytes(Vec<u8>),
  Finish(oneshot::Sender<Result<SealedFile, String>>),
}

/// Where the sealed bytes go.
enum Store {
  Opfs {
    folder: Directory,
    name: String,
    writable: WritableFile,
  },
  Memory {
    parts: Array,
    bytes: u64,
  },
}

/// A finished sealed file.
enum SealedFile {
  Opfs { folder: Directory, name: String },
  Memory(Blob),
}

#[wasm_bindgen]
extern "C" {
  /// `FileSystemDirectoryHandle`.
  #[derive(Clone)]
  type Directory;

  #[wasm_bindgen(method, catch, js_name = getDirectoryHandle)]
  fn get_directory_handle(this: &Directory, name: &str, options: &Object) -> Result<Promise, JsValue>;

  #[wasm_bindgen(method, catch, js_name = getFileHandle)]
  fn get_file_handle(this: &Directory, name: &str, options: &Object) -> Result<Promise, JsValue>;

  #[wasm_bindgen(method, catch, js_name = removeEntry)]
  fn remove_entry(this: &Directory, name: &str, options: &Object) -> Result<Promise, JsValue>;

  /// An async iterator over the names in the folder.
  #[wasm_bindgen(method, catch)]
  fn keys(this: &Directory) -> Result<Object, JsValue>;

  /// `FileSystemFileHandle`.
  type FileEntry;

  #[wasm_bindgen(method, catch, js_name = getFile)]
  fn get_file(this: &FileEntry) -> Result<Promise, JsValue>;

  /// Missing in Safari before 26: such a browser keeps recordings in memory.
  #[wasm_bindgen(method, catch, js_name = createWritable)]
  fn create_writable(this: &FileEntry) -> Result<Promise, JsValue>;

  /// `FileSystemWritableFileStream`.
  type WritableFile;

  #[wasm_bindgen(method, catch)]
  fn write(this: &WritableFile, data: &JsValue) -> Result<Promise, JsValue>;

  #[wasm_bindgen(method, catch)]
  fn close(this: &WritableFile) -> Result<Promise, JsValue>;

  #[wasm_bindgen(method, catch)]
  fn abort(this: &WritableFile) -> Result<Promise, JsValue>;
}

/// Whether this browser can record calls: WebCodecs Opus, which a call needs anyway. Where the
/// file goes (OPFS or memory) is decided when a recording starts.
pub fn is_supported() -> bool {
  has_global("AudioEncoder") && has_global("Blob")
}

impl RecordingFolder {
  /// A folder of its own for this page; opened with the first recording.
  pub fn new() -> Self {
    let page = format!("{:016x}{:016x}", OsRng.next_u64(), OsRng.next_u64());
    Self {
      page,
      maybe_opened: RefCell::new(None),
      next: Cell::new(0),
    }
  }

  /// Deletes the folders of pages that no longer run: a recording such a page left can't be read
  /// (its key died with the page). Without Web Locks, nothing of other pages is deleted, as it
  /// can't be told whether they still run.
  pub async fn sweep() {
    let Some(root) = recordings_root(false).await else {
      return;
    };
    let names = match folder_names(&root).await {
      Ok(names) => names,
      Err(error) => {
        debug!(%error, "the recordings folder could not be listed");
        return;
      },
    };
    if names.is_empty() {
      return;
    }
    let Some(held) = held_locks().await else {
      debug!("no Web Locks: recordings of closed pages are not deleted");
      return;
    };
    for name in names {
      if held.contains(&format!("{LOCK_PREFIX}{name}")) {
        continue;
      }
      if let Err(error) = remove(&root, &name, true).await {
        debug!(%error, "a recording of a closed page was not deleted");
      }
    }
  }

  /// Deletes everything this page recorded (the user signs out, or the call module stops).
  pub async fn clear(&self) {
    let maybe_opened = self.maybe_opened.borrow().clone();
    if !matches!(maybe_opened, Some(Opened::Opfs(_))) {
      return;
    }
    if let Some(root) = recordings_root(false).await
      && let Err(error) = remove(&root, &self.page, true).await
    {
      debug!(%error, "this page's recordings were not deleted");
    }
    // Opened afresh by the next recording.
    self.maybe_opened.replace(None);
  }

  /// A new file for a recording.
  async fn create_store(&self, name_stem: &str) -> Store {
    let number = self.next.get();
    self.next.set(number + 1);
    let name = format!("{name_stem}-{number}");
    if let Opened::Opfs(folder) = self.open().await {
      match create_writable_file(&folder, &name).await {
        Ok(writable) => {
          return Store::Opfs {
            folder,
            name,
            writable,
          };
        },
        Err(error) => warn!(%error, "the recording can't be written to a file, it is kept in memory"),
      }
    }
    Store::Memory {
      parts: Array::new(),
      bytes: 0,
    }
  }

  /// Opens this page's OPFS folder once (taking its lock first, so a sweep never deletes it), or
  /// settles on memory.
  async fn open(&self) -> Opened {
    if let Some(opened) = self.maybe_opened.borrow().clone() {
      return opened;
    }
    let opened = match self.open_opfs().await {
      Ok(folder) => Opened::Opfs(folder),
      Err(error) => {
        info!(%error, "no writable origin private file system: call recordings are kept in memory");
        Opened::Memory
      },
    };
    self.maybe_opened.replace(Some(opened.clone()));
    opened
  }

  async fn open_opfs(&self) -> Result<Directory, String> {
    hold_lock(&format!("{LOCK_PREFIX}{}", self.page)).await;
    let root = recordings_root(true)
      .await
      .ok_or_else(|| "no origin private file system".to_owned())?;
    let options = dictionary(&[("create", JsValue::TRUE)]).map_err(|error| js_message(&error))?;
    let folder = settle(root.get_directory_handle(&self.page, &options)).await?;
    Ok(folder.unchecked_into())
  }
}

impl Default for RecordingFolder {
  fn default() -> Self {
    Self::new()
  }
}

impl WebCallRecorder {
  /// Starts recording into `folder`; `max` is the policy's longest recording. `on_end` is called
  /// once when it ends by itself (its maximum length, or a write that failed); the caller then
  /// finishes it.
  pub async fn start(
    folder: &RecordingFolder,
    max: Duration,
    on_end: Box<dyn Fn()>,
  ) -> Result<Self, RecordingError> {
    if !is_supported() {
      return Err(RecordingError::Unsupported("WebCodecs audio"));
    }
    let store = folder.create_store("recording").await;
    let is_in_memory = matches!(store, Store::Memory { .. });
    let wanted = max.min(MAX_RECORDING_LENGTH);
    let max = if is_in_memory {
      wanted.min(MEMORY_MAX_LENGTH)
    } else {
      wanted
    };
    let is_capped_by_memory = max < wanted;
    let key = SealedStreamKey::generate(&mut OsRng);
    let spool = RecordingSpool::new();
    let sealed = SealedStreamWriter::new(&key, spool.clone(), &mut OsRng)
      .map_err(|error| RecordingError::File(error.to_string()))?;
    let (to_file, from_recorder) = mpsc::unbounded();
    let inner = Rc::new(Inner {
      state: RefCell::new(State {
        mixer: RecordingMixer::new(),
        writer: Writer::Waiting(sealed),
        spool,
        timestamp_us: 0.0,
        frames_encoded: 0,
        frames_written: 0,
        max_frames: max_frames(max),
        maybe_memory_cap: is_in_memory.then_some(MEMORY_MAX_BYTES),
        maybe_end: None,
        is_stopping: false,
        dropped_frames: 0,
      }),
      in_flight: Cell::new(0),
      maybe_to_file: RefCell::new(Some(to_file)),
      maybe_on_end: RefCell::new(Some(on_end)),
    });
    let output = {
      let inner = Rc::clone(&inner);
      Closure::<dyn FnMut(EncodedAudioChunk, JsValue)>::new(
        move |chunk: EncodedAudioChunk, metadata: JsValue| {
          let bytes = Uint8Array::new_with_length(chunk.byte_length());
          match chunk.copy_to(&bytes) {
            Ok(()) => inner.on_packet(bytes.to_vec(), &metadata),
            Err(error) => inner.end(RecordingEnd::Failed(js_message(&error))),
          }
        },
      )
    };
    let error = {
      let inner = Rc::clone(&inner);
      Closure::<dyn FnMut(JsValue)>::new(move |error: JsValue| {
        inner.end(RecordingEnd::Failed(js_message(&error)));
      })
    };
    let init = dictionary(&[
      ("output", output.as_ref().clone()),
      ("error", error.as_ref().clone()),
    ])
    .map_err(browser("AudioEncoder"))?;
    let encoder = AudioEncoder::new(&init).map_err(browser("AudioEncoder"))?;
    encoder
      .configure(&encoder_config()?)
      .map_err(browser("AudioEncoder"))?;
    spawn_local(write_file(store, from_recorder, Rc::clone(&inner)));
    info!(max_secs = max.as_secs(), is_in_memory, "call recording started");
    Ok(Self {
      sink: WebRecordingSink(Rc::new(Shared {
        inner,
        encoder,
        _output: output,
        _error: error,
      })),
      maybe_key: Some(key),
      is_in_memory,
      is_capped_by_memory,
    })
  }

  /// What the call's loop hands its audio to.
  pub fn sink(&self) -> WebRecordingSink {
    self.sink.clone()
  }

  /// Whether the file is kept in memory (no writable OPFS in this browser).
  pub fn is_in_memory(&self) -> bool {
    self.is_in_memory
  }

  /// Stops and finishes the file: what the encoder still holds is written, then the file closes.
  pub async fn finish(mut self) -> Result<WebRecordingTake, RecordingError> {
    let shared = Rc::clone(&self.sink.0);
    shared.inner.state.borrow_mut().is_stopping = true;
    if let Ok(flushed) = shared.encoder.flush() {
      // A closed encoder (it failed) rejects; what came out before counts.
      drop(JsFuture::from(flushed).await);
    }
    drop(shared.encoder.close());
    let (writer, maybe_end, samples) = {
      let mut state = shared.inner.state.borrow_mut();
      let writer = std::mem::replace(&mut state.writer, Writer::Done);
      if state.dropped_frames > 0 {
        info!(
          dropped = state.dropped_frames,
          "call recording dropped frames that came too fast"
        );
      }
      (
        writer,
        state.maybe_end.clone(),
        state.frames_written * FRAME_SAMPLES_PER_CHANNEL as u64,
      )
    };
    if let Some(RecordingEnd::Failed(error)) = maybe_end {
      shared.inner.cancel_file();
      return Err(RecordingError::File(error));
    }
    let size_bytes = match writer {
      Writer::Writing(ogg) if samples > 0 => {
        let sealed = ogg
          .finish()
          .map_err(|error| RecordingError::File(error.to_string()))?;
        let (_, plaintext) = sealed
          .finish()
          .map_err(|error| RecordingError::File(error.to_string()))?;
        plaintext
      },
      _ => {
        shared.inner.cancel_file();
        return Err(RecordingError::Empty);
      },
    };
    shared.inner.send_pending(0);
    let file = shared.inner.close_file().await?;
    let key = self.maybe_key.take().ok_or(RecordingError::Empty)?;
    let duration_ms = samples * 1000 / u64::from(SAMPLE_RATE_HZ);
    let is_at_limit = maybe_end == Some(RecordingEnd::AtLimit);
    info!(duration_secs = duration_ms / 1000, "call recording finished");
    Ok(WebRecordingTake {
      file,
      key,
      duration_ms,
      size_bytes,
      is_at_limit: is_at_limit && !self.is_capped_by_memory,
      is_at_memory_limit: is_at_limit && self.is_capped_by_memory,
    })
  }

  /// Stops and deletes the file.
  pub fn cancel(self) {
    drop(self);
    info!("call recording discarded");
  }
}

impl Drop for WebCallRecorder {
  /// Unfinished: the encoder stops, the file is deleted.
  fn drop(&mut self) {
    let shared = &self.sink.0;
    shared.inner.state.borrow_mut().is_stopping = true;
    drop(shared.encoder.close());
    shared.inner.cancel_file();
    shared.inner.maybe_on_end.replace(None);
  }
}

impl WebRecordingSink {
  /// 20 ms (960 samples) of what this page plays.
  pub fn push_playback(&self, mix: &[f32]) {
    if mix.len() != FRAME_SAMPLES_PER_CHANNEL {
      warn!(
        samples = mix.len(),
        "played frame of an unexpected length skipped"
      );
      return;
    }
    let shared = &self.0;
    let frame = {
      let mut state = shared.inner.state.borrow_mut();
      if state.is_stopping || state.maybe_end.is_some() {
        return;
      }
      // Mixed even when dropped: the own voice stays in step with the played frames.
      let frame = state.mixer.mix(mix);
      let is_behind = shared.inner.in_flight.get() > MAX_IN_FLIGHT_BYTES
        || shared.encoder.encode_queue_size() > MAX_ENCODE_QUEUE;
      if is_behind {
        state.dropped_frames += 1;
        if state.dropped_frames.is_power_of_two() {
          debug!(
            dropped = state.dropped_frames,
            "call recording falls behind, frames dropped"
          );
        }
        return;
      }
      let timestamp_us = state.timestamp_us;
      state.timestamp_us += 20_000.0;
      (frame, timestamp_us)
    };
    match shared.encode(&frame.0, frame.1) {
      Ok(()) => shared.inner.state.borrow_mut().frames_encoded += 1,
      Err(error) => shared.inner.end(RecordingEnd::Failed(error.to_string())),
    }
  }

  /// The own voice as the call's encoder gets it (none while muted, so it isn't recorded).
  pub fn push_own(&self, samples: &[f32]) {
    self.0.inner.state.borrow_mut().mixer.push_own(samples);
  }

  /// Whether the recording takes nothing more (its maximum, or a failed write): the call stops
  /// handing it audio and tells the others.
  pub fn is_ended(&self) -> bool {
    self.0.inner.state.borrow().maybe_end.is_some()
  }
}

impl Shared {
  fn encode(&self, frame: &[f32], timestamp_us: f64) -> Result<(), RecordingError> {
    let data = AudioData::new(
      &dictionary(&[
        ("format", JsValue::from_str("f32-planar")),
        ("sampleRate", JsValue::from(SAMPLE_RATE_HZ)),
        ("numberOfFrames", JsValue::from(FRAME_SAMPLES_PER_CHANNEL as u32)),
        ("numberOfChannels", JsValue::from(1)),
        ("timestamp", JsValue::from(timestamp_us)),
        ("data", Float32Array::from(frame).into()),
      ])
      .map_err(browser("AudioData"))?,
    )
    .map_err(browser("AudioData"))?;
    let result = self.encoder.encode(&data);
    data.close();
    result.map_err(browser("AudioEncoder"))
  }
}

impl Inner {
  /// One encoded 20 ms packet: into the file, and the full pieces on to the file task.
  fn on_packet(&self, packet: Vec<u8>, metadata: &JsValue) {
    let maybe_end = {
      let mut state = self.state.borrow_mut();
      let is_extra = state.frames_written >= state.frames_encoded;
      if is_extra
        || state.frames_written >= state.max_frames
        || matches!(state.maybe_end, Some(RecordingEnd::Failed(_)))
      {
        return;
      }
      if let Writer::Waiting(_) = state.writer
        && let Writer::Waiting(sealed) = std::mem::replace(&mut state.writer, Writer::Done)
      {
        let pre_skip = description_of(metadata)
          .and_then(|description| pre_skip_of(&description))
          .unwrap_or(DEFAULT_PRE_SKIP);
        match OggOpusWriter::new(sealed, pre_skip) {
          Ok(ogg) => state.writer = Writer::Writing(ogg),
          Err(error) => return self.end_locked(state, RecordingEnd::Failed(error.to_string())),
        }
      }
      let Writer::Writing(ogg) = &mut state.writer else {
        return;
      };
      if let Err(error) = ogg.push(packet, FRAME_SAMPLES_PER_CHANNEL as u32) {
        return self.end_locked(state, RecordingEnd::Failed(error.to_string()));
      }
      state.frames_written += 1;
      let is_full = state
        .maybe_memory_cap
        .is_some_and(|cap| state.spool.written() >= cap);
      (state.frames_written >= state.max_frames || is_full).then_some(RecordingEnd::AtLimit)
    };
    self.send_pending(WRITE_BYTES);
    if let Some(end) = maybe_end {
      self.end(end);
    }
  }

  /// Ends the recording by itself, once; the module is called back to finish it.
  fn end(&self, end: RecordingEnd) {
    let state = self.state.borrow_mut();
    self.end_locked(state, end);
  }

  fn end_locked(&self, mut state: std::cell::RefMut<'_, State>, end: RecordingEnd) {
    if state.maybe_end.is_some() {
      return;
    }
    if let RecordingEnd::Failed(error) = &end {
      warn!(%error, "the call recording failed");
    }
    state.maybe_end = Some(end);
    let is_stopping = state.is_stopping;
    drop(state);
    if !is_stopping && let Some(on_end) = self.maybe_on_end.borrow_mut().take() {
      on_end();
    }
  }

  /// Sends what is pending to the file once it reaches `at_least` bytes (0: whatever is there).
  fn send_pending(&self, at_least: usize) {
    let Some(bytes) = self.state.borrow().spool.take(at_least) else {
      return;
    };
    let length = bytes.len();
    if let Some(to_file) = self.maybe_to_file.borrow().as_ref()
      && to_file.unbounded_send(ToFile::Bytes(bytes)).is_ok()
    {
      self.in_flight.set(self.in_flight.get() + length);
    }
  }

  /// Waits for the file task to write what was sent and close the file.
  async fn close_file(&self) -> Result<SealedFile, RecordingError> {
    let (done, closed) = oneshot::channel();
    let maybe_to_file = self.maybe_to_file.borrow_mut().take();
    let to_file = maybe_to_file.ok_or_else(|| RecordingError::File("the file was closed".to_owned()))?;
    to_file
      .unbounded_send(ToFile::Finish(done))
      .map_err(|_| RecordingError::File("the file task ended".to_owned()))?;
    closed
      .await
      .map_err(|_| RecordingError::File("the file task ended".to_owned()))?
      .map_err(RecordingError::File)
  }

  /// The file task deletes the file once the queue closes without a finish.
  fn cancel_file(&self) {
    drop(self.maybe_to_file.borrow_mut().take());
  }
}

impl WebRecordingTake {
  /// The decrypted Ogg file, written next to the sealed one in `folder` (or put together in memory
  /// where the browser keeps recordings there), for the upload. Streams: never the whole file in
  /// memory where it is on disk. A take that doesn't decrypt (changed meanwhile) fails, and what was
  /// written of the copy goes. The take stays: the module deletes it once the copy is handed over,
  /// or keeps offering it.
  pub async fn decrypt(&self, folder: &RecordingFolder) -> Result<Blob, RecordingError> {
    let sealed = self.file.blob().await.map_err(RecordingError::File)?;
    let mut output = folder.create_store("post").await;
    match self.decrypt_into(&sealed, &mut output).await {
      Ok(()) => output.into_blob().await.map_err(RecordingError::File),
      Err(error) => {
        output.discard().await;
        Err(error)
      },
    }
  }

  /// Deletes the sealed file; the key goes with the take.
  pub async fn delete(self) {
    self.file.remove().await;
  }

  async fn decrypt_into(&self, sealed: &Blob, output: &mut Store) -> Result<(), RecordingError> {
    let total = sealed.size() as u64;
    let source = PrefetchedBytes::new();
    let mut offset = 0u64;
    let fail =
      |error: std::io::Error| RecordingError::File(format!("the recording doesn't decrypt: {error}"));
    top_up(sealed, &source, &mut offset, total).await?;
    let mut reader = SealedStreamReader::new(&self.key, source.clone()).map_err(fail)?;
    let mut buf = vec![0u8; CHUNK_SIZE as usize];
    let mut pending = Vec::with_capacity(COPY_BYTES + CHUNK_SIZE as usize);
    let mut written = 0u64;
    loop {
      top_up(sealed, &source, &mut offset, total).await?;
      let read = std::io::Read::read(&mut reader, &mut buf).map_err(fail)?;
      if read == 0 {
        break;
      }
      pending.extend_from_slice(&buf[..read]);
      if pending.len() >= COPY_BYTES {
        written += pending.len() as u64;
        output.write(&pending).await.map_err(RecordingError::File)?;
        pending.clear();
      }
    }
    if !pending.is_empty() {
      written += pending.len() as u64;
      output.write(&pending).await.map_err(RecordingError::File)?;
    }
    if written != self.size_bytes {
      return Err(RecordingError::File(
        "the recording decrypted to another size".to_owned(),
      ));
    }
    Ok(())
  }
}

impl SealedFile {
  async fn blob(&self) -> Result<Blob, String> {
    match self {
      Self::Opfs { folder, name } => {
        let options = Object::new();
        let entry: FileEntry = settle(folder.get_file_handle(name, &options))
          .await?
          .unchecked_into();
        Ok(settle(entry.get_file()).await?.unchecked_into())
      },
      Self::Memory(blob) => Ok(blob.clone()),
    }
  }

  async fn remove(self) {
    if let Self::Opfs { folder, name } = self
      && let Err(error) = remove(&folder, &name, false).await
    {
      debug!(%error, "a call recording was not deleted");
    }
  }
}

impl Store {
  async fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
    match self {
      Self::Opfs { writable, .. } => {
        let data = Uint8Array::from(bytes);
        settle(writable.write(&data.into())).await.map(drop)
      },
      Self::Memory { parts, bytes: kept } => {
        // The recorder stops at the cap; this is the backstop against a runaway writer.
        if *kept + bytes.len() as u64 > MEMORY_MAX_BYTES + 2 * WRITE_BYTES as u64 {
          return Err("the recording is too large to keep in memory".to_owned());
        }
        let data = Uint8Array::from(bytes);
        let blob =
          Blob::new_with_u8_array_sequence(&Array::of1(&data)).map_err(|error| js_message(&error))?;
        parts.push(&blob);
        *kept += bytes.len() as u64;
        Ok(())
      },
    }
  }

  /// Closes the file and names it.
  async fn close(self) -> Result<SealedFile, String> {
    match self {
      Self::Opfs {
        folder,
        name,
        writable,
      } => match settle(writable.close()).await {
        Ok(_) => Ok(SealedFile::Opfs { folder, name }),
        Err(error) => {
          drop(remove(&folder, &name, false).await);
          Err(error)
        },
      },
      Self::Memory { parts, .. } => Ok(SealedFile::Memory(memory_blob(&parts, None)?)),
    }
  }

  /// Closes the file and hands it over as a blob (an Ogg file).
  async fn into_blob(self) -> Result<Blob, String> {
    match self {
      Self::Opfs { .. } => {
        let file = self.close().await?;
        file.blob().await
      },
      Self::Memory { parts, .. } => memory_blob(&parts, Some("audio/ogg")),
    }
  }

  /// Ends the file without keeping it.
  async fn discard(self) {
    if let Self::Opfs {
      folder,
      name,
      writable,
    } = self
    {
      if let Ok(aborted) = writable.abort() {
        drop(JsFuture::from(aborted).await);
      }
      if let Err(error) = remove(&folder, &name, false).await {
        debug!(%error, "a call recording was not deleted");
      }
    }
  }
}

/// The file task: writes what the recorder sends, in order, then closes the file, or deletes it
/// when the queue closes without a finish (cancelled). A write that fails (the quota is used up)
/// ends the recording at once, so the others are told it stopped: the rest is dropped and the
/// finish answers with the failure.
async fn write_file(
  mut store: Store,
  mut from_recorder: mpsc::UnboundedReceiver<ToFile>,
  recorder: Rc<Inner>,
) {
  let mut maybe_failure: Option<String> = None;
  while let Some(message) = from_recorder.next().await {
    match message {
      ToFile::Bytes(bytes) => {
        if maybe_failure.is_none()
          && let Err(error) = store.write(&bytes).await
        {
          recorder.end(RecordingEnd::Failed(error.clone()));
          maybe_failure = Some(error);
        }
        recorder
          .in_flight
          .set(recorder.in_flight.get().saturating_sub(bytes.len()));
      },
      ToFile::Finish(done) => {
        let result = match maybe_failure {
          Some(error) => {
            store.discard().await;
            Err(error)
          },
          None => store.close().await,
        };
        let _ = done.send(result);
        return;
      },
    }
  }
  store.discard().await;
}

/// Fetches slices of `sealed` until [`READ_AHEAD`] bytes wait or the file is read to its end.
async fn top_up(
  sealed: &Blob,
  source: &PrefetchedBytes,
  offset: &mut u64,
  total: u64,
) -> Result<(), RecordingError> {
  while source.len() < READ_AHEAD && !source.is_at_end() {
    if *offset >= total {
      source.end();
      break;
    }
    let end = (*offset + READ_SLICE as u64).min(total);
    // Invariant: file sizes are far below 2^53, so the offsets are exact as f64.
    let slice = sealed
      .slice_with_f64_and_f64(*offset as f64, end as f64)
      .map_err(|error| RecordingError::File(js_message(&error)))?;
    let buffer = JsFuture::from(slice.array_buffer())
      .await
      .map_err(|error| RecordingError::File(js_message(&error)))?;
    source.push(&Uint8Array::new(&buffer).to_vec());
    *offset = end;
  }
  Ok(())
}

/// The Opus encoder of a recording: like the desktop's, no FEC and no DTX (a file has no losses to
/// plan for, and gaps of silence would break its timing).
fn encoder_config() -> Result<Object, RecordingError> {
  let opus = dictionary(&[
    ("frameDuration", JsValue::from(20_000)),
    ("useinbandfec", JsValue::FALSE),
    ("packetlossperc", JsValue::from(0)),
    ("usedtx", JsValue::FALSE),
  ])
  .map_err(browser("AudioEncoder"))?;
  dictionary(&[
    ("codec", JsValue::from_str("opus")),
    ("sampleRate", JsValue::from(SAMPLE_RATE_HZ)),
    ("numberOfChannels", JsValue::from(1)),
    ("bitrate", JsValue::from(RECORDING_BITRATE_BPS)),
    ("opus", opus.into()),
  ])
  .map_err(browser("AudioEncoder"))
}

/// `metadata.decoderConfig.description` of an encoder output, where the browser gives one.
fn description_of(metadata: &JsValue) -> Option<Vec<u8>> {
  let config = Reflect::get(metadata, &JsValue::from_str("decoderConfig")).ok()?;
  let description = Reflect::get(&config, &JsValue::from_str("description")).ok()?;
  if description.is_undefined() || description.is_null() {
    return None;
  }
  let bytes = if let Some(view) = description.dyn_ref::<js_sys::DataView>() {
    Uint8Array::new_with_byte_offset_and_length(
      &view.buffer(),
      view.byte_offset() as u32,
      view.byte_length() as u32,
    )
  } else if description.is_instance_of::<js_sys::ArrayBuffer>() || description.is_instance_of::<Uint8Array>()
  {
    Uint8Array::new(&description)
  } else {
    return None;
  };
  Some(bytes.to_vec())
}

/// The origin's folder of recordings; created when asked to.
async fn recordings_root(create: bool) -> Option<Directory> {
  let window = web_sys::window()?;
  let storage = Reflect::get(&window.navigator(), &JsValue::from_str("storage")).ok()?;
  let get_directory = Reflect::get(&storage, &JsValue::from_str("getDirectory"))
    .ok()?
    .dyn_into::<js_sys::Function>()
    .ok()?;
  let root = match get_directory.call0(&storage) {
    Ok(promise) => JsFuture::from(Promise::from(promise)).await.ok()?,
    Err(_) => return None,
  };
  let root: Directory = root.unchecked_into();
  let options = dictionary(&[("create", JsValue::from_bool(create))]).ok()?;
  settle(root.get_directory_handle(FOLDER, &options))
    .await
    .ok()
    .map(JsCast::unchecked_into)
}

/// A new file in `folder`, open for writing. Fails where the browser can't write OPFS files from
/// the page (no `createWritable`).
async fn create_writable_file(folder: &Directory, name: &str) -> Result<WritableFile, String> {
  let options = dictionary(&[("create", JsValue::TRUE)]).map_err(|error| js_message(&error))?;
  let entry: FileEntry = settle(folder.get_file_handle(name, &options))
    .await?
    .unchecked_into();
  match settle(entry.create_writable()).await {
    Ok(writable) => Ok(writable.unchecked_into()),
    Err(error) => {
      drop(remove(folder, name, false).await);
      Err(error)
    },
  }
}

async fn folder_names(folder: &Directory) -> Result<Vec<String>, String> {
  let iterator = folder.keys().map_err(|error| js_message(&error))?;
  let next = Reflect::get(&iterator, &JsValue::from_str("next"))
    .and_then(|next| next.dyn_into::<js_sys::Function>())
    .map_err(|error| js_message(&error))?;
  let mut names = Vec::new();
  loop {
    let step = next.call0(&iterator).map_err(|error| js_message(&error))?;
    let step = JsFuture::from(Promise::from(step))
      .await
      .map_err(|error| js_message(&error))?;
    if Reflect::get(&step, &JsValue::from_str("done"))
      .ok()
      .is_some_and(|done| done.is_truthy())
    {
      return Ok(names);
    }
    if let Some(name) = Reflect::get(&step, &JsValue::from_str("value"))
      .ok()
      .and_then(|value| value.as_string())
    {
      names.push(name);
    }
  }
}

async fn remove(folder: &Directory, name: &str, is_folder: bool) -> Result<(), String> {
  let options =
    dictionary(&[("recursive", JsValue::from_bool(is_folder))]).map_err(|error| js_message(&error))?;
  match settle(folder.remove_entry(name, &options)).await {
    Ok(_) => Ok(()),
    Err(error) if error.contains("NotFound") || error.contains("could not be found") => Ok(()),
    Err(error) => Err(error),
  }
}

/// `navigator.locks`, where the browser has Web Locks.
fn locks() -> Option<JsValue> {
  let window = web_sys::window()?;
  let locks = Reflect::get(&window.navigator(), &JsValue::from_str("locks")).ok()?;
  locks.is_object().then_some(locks)
}

/// Takes the lock `name` for as long as the page lives; returns once it is held (at once, as no
/// other page uses the name). Without Web Locks, returns at once.
async fn hold_lock(name: &str) {
  let Some(locks) = locks() else {
    return;
  };
  let Ok(request) = Reflect::get(&locks, &JsValue::from_str("request"))
    .and_then(|request| request.dyn_into::<js_sys::Function>())
  else {
    return;
  };
  let (granted, is_granted) = oneshot::channel::<()>();
  // Called once with the lock; the promise it returns never settles, so the lock stays held.
  let callback = Closure::once_into_js(move |_lock: JsValue| -> Promise {
    let _ = granted.send(());
    Promise::new(&mut |_, _| {})
  });
  if request
    .call2(&locks, &JsValue::from_str(name), &callback)
    .is_err()
  {
    return;
  }
  let _ = is_granted.await;
}

/// The names of the locks held now in the origin; `None` without Web Locks.
async fn held_locks() -> Option<Vec<String>> {
  let locks = locks()?;
  let query = Reflect::get(&locks, &JsValue::from_str("query"))
    .ok()?
    .dyn_into::<js_sys::Function>()
    .ok()?;
  let snapshot = JsFuture::from(Promise::from(query.call0(&locks).ok()?))
    .await
    .ok()?;
  let held: Array = Reflect::get(&snapshot, &JsValue::from_str("held"))
    .ok()?
    .dyn_into()
    .ok()?;
  Some(
    held
      .iter()
      .filter_map(|lock| Reflect::get(&lock, &JsValue::from_str("name")).ok()?.as_string())
      .collect(),
  )
}

fn memory_blob(parts: &Array, maybe_type: Option<&str>) -> Result<Blob, String> {
  let bag = BlobPropertyBag::new();
  if let Some(media_type) = maybe_type {
    bag.set_type(media_type);
  }
  Blob::new_with_blob_sequence_and_options(parts, &bag).map_err(|error| js_message(&error))
}

/// Awaits a promise that a browser call may also refuse synchronously.
async fn settle(result: Result<Promise, JsValue>) -> Result<JsValue, String> {
  let promise = result.map_err(|error| js_message(&error))?;
  JsFuture::from(promise).await.map_err(|error| {
    let name = Reflect::get(&error, &JsValue::from_str("name"))
      .ok()
      .and_then(|name| name.as_string())
      .unwrap_or_default();
    format!("{name}: {}", js_message(&error))
  })
}

fn browser(context: &'static str) -> impl Fn(JsValue) -> RecordingError {
  move |error| RecordingError::Browser {
    context,
    message: js_message(&error),
  }
}
