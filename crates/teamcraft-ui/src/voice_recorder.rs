//! The app's one voice recording (#204): what the composer's record button starts, its level and
//! length while it runs, and the take the user sends or discards.
//!
//! - In the browser the page records: `getUserMedia` and `MediaRecorder` (Opus in Ogg where the
//!   browser writes it, Firefox; Opus in WebM in Chrome and Edge; MP4 in Safari), with an
//!   `AnalyserNode` for the level and the waveform. The take is a `Blob` in the page until sent.
//! - In the desktop app the shell records (`voice.rs` there; the webview doesn't capture) into its
//!   private folder; the composer polls its level and length ten times a second while it shows
//!   the recording.
//! - One recording at a time for the whole app, kept here and not in the composer, so a take
//!   survives leaving the conversation and coming back. A call, the app going to the background
//!   or leaving the conversation stops a recording and keeps the take for review; nothing is
//!   ever sent without the user's press.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use leptos::leptos_dom::helpers::{IntervalHandle, set_interval_with_handle};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use teamcraft_client_api::actions::{FileSource, OutgoingFile};
use teamcraft_client_api::voice::{
  self, RecordingRefusal, RecordingStatus, RecordingStop, ShellTake, Waveform,
};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::JsFuture;

use crate::auth::tauri;
use crate::large_files::{self, LargeFile, READ_WHOLE_MAX_BYTES};
use crate::model::{ConversationId, ThreadId};
use crate::settings;

/// How often the level and length are read while recording.
const TICK: Duration = Duration::from_millis(100);
/// Recent levels the live meter shows.
const LIVE_BARS: usize = 40;
/// Remembers that the microphone explanation was shown on this device.
const EXPLAINED_KEY: &str = "voice-microphone-explained";
/// The browser formats in the order they are preferred: Opus first (#204).
const BROWSER_FORMATS: [BrowserFormat; 3] = [
  BrowserFormat {
    mime_type: "audio/ogg;codecs=opus",
    media_type: "audio/ogg",
    extension: "ogg",
  },
  BrowserFormat {
    mime_type: "audio/webm;codecs=opus",
    media_type: "audio/webm",
    extension: "webm",
  },
  BrowserFormat {
    mime_type: "audio/mp4",
    media_type: "audio/mp4",
    extension: "m4a",
  },
];
/// Speech needs no more; the browser may pick less.
const BROWSER_BITRATE_BPS: u32 = 32_000;

/// The recorder. One handle, copied into closures like [`crate::audio_player::AudioPlayer`].
#[derive(Clone, Copy)]
pub struct VoiceRecorder(StoredValue<Parts>);

/// Which composer a recording belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComposerKey {
  pub conversation: ConversationId,
  pub maybe_thread: Option<ThreadId>,
}

/// Where the recording stands.
#[derive(Clone, Debug, PartialEq)]
pub enum RecorderPhase {
  /// The first press on this device: the composer explains what the microphone is for first.
  Explaining,
  /// Opening the microphone.
  Starting,
  Recording,
  /// Stopping and finishing the file.
  Finishing,
  /// A take to send or discard.
  Review(TakeInfo),
  /// The microphone didn't open: the composer says why and how to allow it.
  Refused(RecordingRefusal),
}

/// What the composer shows of a take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TakeInfo {
  pub duration_ms: u64,
  pub waveform: Vec<u8>,
  pub maybe_stop: Option<RecordingStop>,
}

#[derive(Clone, Copy)]
struct Parts {
  maybe_owner: RwSignal<Option<ComposerKey>>,
  maybe_phase: RwSignal<Option<RecorderPhase>>,
  /// The meter's level, 0.0 to 1.0, and the recent ones for the live bars.
  level: RwSignal<f32>,
  live: RwSignal<Vec<f32>>,
  elapsed_ms: RwSignal<u64>,
  /// The maximum length of the recording under way.
  max_ms: StoredValue<u64>,
  /// The take's bytes or file, until sent or discarded.
  maybe_source: StoredValue<Option<TakeSource>, LocalStorage>,
  maybe_browser: StoredValue<Option<BrowserRecording>, LocalStorage>,
  maybe_ticker: StoredValue<Option<IntervalHandle>, LocalStorage>,
  /// Bumped by every start and discard: a late answer for an older recording is dropped.
  generation: StoredValue<u64>,
}

/// Where a take is.
#[derive(Clone)]
enum TakeSource {
  Browser {
    blob: web_sys::Blob,
    format: BrowserFormat,
  },
  Shell {
    path: String,
  },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BrowserFormat {
  mime_type: &'static str,
  media_type: &'static str,
  extension: &'static str,
}

/// A recording in the page.
struct BrowserRecording {
  recorder: web_sys::MediaRecorder,
  stream: web_sys::MediaStream,
  context: web_sys::AudioContext,
  analyser: web_sys::AnalyserNode,
  chunks: Rc<RefCell<Vec<web_sys::Blob>>>,
  levels: Vec<f32>,
  format: BrowserFormat,
  started_at_ms: f64,
  _on_data: Closure<dyn FnMut(web_sys::BlobEvent)>,
}

impl VoiceRecorder {
  pub fn new() -> Self {
    Self(StoredValue::new(Parts {
      maybe_owner: RwSignal::new(None),
      maybe_phase: RwSignal::new(None),
      level: RwSignal::new(0.0),
      live: RwSignal::new(Vec::new()),
      elapsed_ms: RwSignal::new(0),
      max_ms: StoredValue::new(0),
      maybe_source: StoredValue::new_local(None),
      maybe_browser: StoredValue::new_local(None),
      maybe_ticker: StoredValue::new_local(None),
      generation: StoredValue::new(0),
    }))
  }

  /// Whether this app can record at all: the desktop app, or a browser with `MediaRecorder`.
  pub fn is_supported() -> bool {
    tauri::is_available() || browser_format().is_some()
  }

  /// The phase of `key`'s recording; `None` when it has none.
  pub fn phase_for(self, key: ComposerKey) -> Option<RecorderPhase> {
    let parts = self.parts()?;
    (parts.maybe_owner.get() == Some(key))
      .then(|| parts.maybe_phase.get())
      .flatten()
  }

  /// Another composer holds the recording (or its take).
  pub fn is_busy_elsewhere(self, key: ComposerKey) -> bool {
    self.parts().is_some_and(|parts| {
      parts.maybe_phase.with(Option::is_some) && parts.maybe_owner.get().is_some_and(|owner| owner != key)
    })
  }

  pub fn level(self) -> f32 {
    self.parts().map_or(0.0, |parts| parts.level.get())
  }

  pub fn live_levels(self) -> Vec<f32> {
    self.parts().map(|parts| parts.live.get()).unwrap_or_default()
  }

  pub fn elapsed_ms(self) -> u64 {
    self.parts().map_or(0, |parts| parts.elapsed_ms.get())
  }

  /// The press on the record button: the explanation the first time, then the recording.
  pub fn press(self, key: ComposerKey, max_seconds: u32) {
    let Some(parts) = self.parts() else {
      return;
    };
    if parts.maybe_phase.get_untracked().is_some() {
      return;
    }
    parts.maybe_owner.set(Some(key));
    match settings::load_string(EXPLAINED_KEY) {
      Some(_) => self.start(max_seconds),
      None => parts.maybe_phase.set(Some(RecorderPhase::Explaining)),
    }
  }

  /// After the explanation (or a refusal): asks for the microphone and records.
  pub fn start(self, max_seconds: u32) {
    let Some(parts) = self.parts() else {
      return;
    };
    settings::store_string(EXPLAINED_KEY, "1");
    parts.generation.update_value(|generation| *generation += 1);
    let generation = parts.generation.get_value();
    parts.maybe_phase.set(Some(RecorderPhase::Starting));
    parts.max_ms.set_value(u64::from(max_seconds) * 1000);
    parts.elapsed_ms.set(0);
    parts.level.set(0.0);
    parts.live.set(Vec::new());
    spawn_local(async move {
      let started = match tauri::is_available() {
        true => start_in_shell(max_seconds).await,
        false => start_in_browser().await.map(|recording| {
          parts.maybe_browser.try_set_value(Some(recording));
        }),
      };
      if parts.generation.try_get_value() != Some(generation) {
        // Discarded while the microphone opened.
        self.release_microphone().await;
        return;
      }
      match started {
        Ok(()) => {
          parts.maybe_phase.try_set(Some(RecorderPhase::Recording));
          self.start_ticker();
        },
        Err(refusal) => drop(parts.maybe_phase.try_set(Some(RecorderPhase::Refused(refusal)))),
      }
    });
  }

  /// Stops the recording and shows the take. `maybe_reason`: why, when the user didn't.
  pub fn stop(self, maybe_reason: Option<RecordingStop>) {
    spawn_local(async move {
      drop(self.finish(maybe_reason).await);
    });
  }

  /// Stops a recording that runs (a call, the background, leaving); a take stays as it is.
  pub fn interrupt(self, reason: RecordingStop) {
    let is_recording = self
      .parts()
      .and_then(|parts| parts.maybe_phase.try_get_untracked().flatten())
      .is_some_and(|phase| matches!(phase, RecorderPhase::Starting | RecorderPhase::Recording));
    if is_recording {
      self.stop(Some(reason));
    }
  }

  /// Leaves the recording or take: deleted, nothing sent.
  pub fn discard(self) {
    let Some(parts) = self.parts() else {
      return;
    };
    parts.generation.update_value(|generation| *generation += 1);
    self.stop_ticker();
    let maybe_source = parts.maybe_source.try_update_value(Option::take).flatten();
    parts.maybe_phase.set(None);
    parts.maybe_owner.set(None);
    spawn_local(async move {
      self.release_microphone().await;
      if let Some(TakeSource::Shell { path }) = maybe_source
        && let Err(error) = tauri::call::<()>("voice_take_discard", &json!({ "path": path })).await
      {
        tracing::debug!(%error, "a discarded voice take was not deleted");
      }
    });
  }

  /// The take as a file for the message (stopping the recording first if it runs), and the
  /// recorder free again. `Ok(None)` while a hand-over already runs. On `Err` (a reason for the
  /// log) the take stays for another try.
  pub async fn take_file(self) -> Result<Option<OutgoingFile>, String> {
    let parts = self.parts().ok_or("the recorder is gone")?;
    let info = match parts.maybe_phase.get_untracked() {
      Some(RecorderPhase::Review(info)) => info,
      Some(RecorderPhase::Starting | RecorderPhase::Recording) => self.finish(None).await?,
      _ => return Ok(None),
    };
    let source = parts
      .maybe_source
      .try_get_value()
      .flatten()
      .ok_or("the take is gone")?;
    let generation = parts.generation.try_get_value().ok_or("the recorder is gone")?;
    parts.maybe_phase.try_set(Some(RecorderPhase::Finishing));
    match hand_over(source).await {
      Ok((file_name, media_type, source)) => {
        parts.maybe_source.try_set_value(None);
        parts.maybe_phase.try_set(None);
        parts.maybe_owner.try_set(None);
        Ok(Some(OutgoingFile {
          file_name,
          media_type: media_type.to_owned(),
          source,
          maybe_thumbnail: None,
          maybe_duration_ms: Some(info.duration_ms),
          maybe_waveform: Some(info.waveform),
        }))
      },
      Err(error) => {
        // Back to review, unless the take was discarded (or the user signed out) meanwhile.
        if parts.generation.try_get_value() == Some(generation) {
          parts.maybe_phase.try_set(Some(RecorderPhase::Review(info)));
        }
        Err(error)
      },
    }
  }

  /// Ends whatever runs at once and deletes the take: the signed-in app goes away (sign-out,
  /// another account). Works from a cleanup, where the reactive values may already be gone: a
  /// recording in the page stops its microphone when dropped, and the shell is told to cancel.
  pub fn release_now(self) {
    self.stop_ticker();
    let maybe_parts = self.parts();
    let maybe_source =
      maybe_parts.and_then(|parts| parts.maybe_source.try_update_value(Option::take).flatten());
    drop(maybe_parts.and_then(|parts| parts.maybe_browser.try_update_value(Option::take).flatten()));
    if let Some(parts) = maybe_parts {
      parts.generation.try_update_value(|generation| *generation += 1);
      parts.maybe_phase.try_set(None);
      parts.maybe_owner.try_set(None);
    }
    if tauri::is_available() {
      spawn_local(async move {
        drop(tauri::call::<()>("voice_record_cancel", &json!({})).await);
        if let Some(TakeSource::Shell { path }) = maybe_source {
          drop(tauri::call::<()>("voice_take_discard", &json!({ "path": path })).await);
        }
      });
    }
  }

  /// Stops `key`'s recording when its composer goes (the thread panel closed); its take stays.
  pub fn interrupt_for(self, key: ComposerKey, reason: RecordingStop) {
    let is_owner = self
      .parts()
      .and_then(|parts| parts.maybe_owner.try_get_untracked().flatten())
      .is_some_and(|owner| owner == key);
    if is_owner {
      self.interrupt(reason);
    }
  }

  /// Stops the recording and keeps the take; returns what the composer shows of it.
  async fn finish(self, maybe_reason: Option<RecordingStop>) -> Result<TakeInfo, String> {
    let parts = self.parts().ok_or("the recorder is gone")?;
    match parts.maybe_phase.get_untracked() {
      Some(RecorderPhase::Recording) => {},
      // Nothing recorded yet: stopped while the microphone opened is a discard.
      Some(RecorderPhase::Starting) => {
        self.discard();
        return Err("stopped before the recording started".to_owned());
      },
      _ => return Err("not recording".to_owned()),
    }
    let generation = parts.generation.get_value();
    parts.maybe_phase.set(Some(RecorderPhase::Finishing));
    self.stop_ticker();
    let finished = match tauri::is_available() {
      true => finish_in_shell(maybe_reason).await,
      false => match parts.maybe_browser.try_update_value(Option::take).flatten() {
        Some(recording) => recording.finish(maybe_reason).await,
        None => Err("no recording in the page".to_owned()),
      },
    };
    if parts.generation.try_get_value() != Some(generation) {
      return Err("discarded".to_owned());
    }
    match finished {
      Ok((info, source)) => {
        parts.maybe_source.try_set_value(Some(source));
        parts
          .maybe_phase
          .try_set(Some(RecorderPhase::Review(info.clone())));
        parts.level.try_set(0.0);
        Ok(info)
      },
      Err(error) => {
        tracing::debug!(%error, "the voice recording ended without a take");
        parts.maybe_phase.try_set(None);
        parts.maybe_owner.try_set(None);
        Err(error)
      },
    }
  }

  /// Reads the level and length ten times a second while recording.
  fn start_ticker(self) {
    let Some(parts) = self.parts() else {
      return;
    };
    let tick = move || self.tick();
    match set_interval_with_handle(tick, TICK) {
      Ok(handle) => parts.maybe_ticker.set_value(Some(handle)),
      Err(_) => tracing::debug!("could not start the voice recording's ticker"),
    }
  }

  fn stop_ticker(self) {
    if let Some(handle) = self
      .parts()
      .and_then(|parts| parts.maybe_ticker.try_update_value(Option::take).flatten())
    {
      handle.clear();
    }
  }

  fn tick(self) {
    let Some(parts) = self.parts() else {
      return;
    };
    if tauri::is_available() {
      spawn_local(async move {
        match tauri::call::<Option<RecordingStatus>>("voice_record_status", &json!({})).await {
          Ok(Some(status)) => {
            self.show(status.level, status.elapsed_ms);
            if let Some(reason) = status.maybe_stop {
              self.stop(Some(reason));
            }
          },
          Ok(None) => self.stop(Some(RecordingStop::MicrophoneLost)),
          Err(error) => tracing::debug!(%error, "the voice recording's status was not read"),
        }
      });
      return;
    }
    let max_ms = parts.max_ms.get_value();
    let maybe_reading = parts
      .maybe_browser
      .try_update_value(|maybe_recording| maybe_recording.as_mut().map(BrowserRecording::read))
      .flatten();
    let Some((level, elapsed_ms, is_live)) = maybe_reading else {
      return;
    };
    self.show(level, elapsed_ms);
    if !is_live {
      self.stop(Some(RecordingStop::MicrophoneLost));
    } else if elapsed_ms >= max_ms {
      self.stop(Some(RecordingStop::LimitReached));
    }
  }

  fn show(self, level: f32, elapsed_ms: u64) {
    let Some(parts) = self.parts() else {
      return;
    };
    parts.level.try_set(level);
    parts.elapsed_ms.try_set(elapsed_ms);
    parts.live.try_update(|live| {
      live.push(level);
      if live.len() > LIVE_BARS {
        live.remove(0);
      }
    });
  }

  /// Lets go of a microphone a discarded recording still holds.
  async fn release_microphone(self) {
    if tauri::is_available() {
      if let Err(error) = tauri::call::<()>("voice_record_cancel", &json!({})).await {
        tracing::debug!(%error, "the voice recording was not cancelled");
      }
    } else if let Some(recording) = self
      .parts()
      .and_then(|parts| parts.maybe_browser.try_update_value(Option::take).flatten())
    {
      recording.release();
    }
  }

  fn parts(self) -> Option<Parts> {
    self.0.try_get_value()
  }
}

impl Default for VoiceRecorder {
  fn default() -> Self {
    Self::new()
  }
}

impl BrowserRecording {
  /// Level, length and whether the microphone still delivers; the level goes into the waveform.
  fn read(&mut self) -> (f32, u64, bool) {
    let mut samples = vec![0.0f32; self.analyser.fft_size() as usize];
    self.analyser.get_float_time_domain_data(&mut samples);
    let rms =
      (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len().max(1) as f32).sqrt();
    self.levels.push(rms);
    let elapsed_ms = (now() - self.started_at_ms).max(0.0) as u64;
    let is_live = self
      .stream
      .get_audio_tracks()
      .iter()
      .filter_map(|track| track.dyn_into::<web_sys::MediaStreamTrack>().ok())
      .any(|track| track.ready_state() == web_sys::MediaStreamTrackState::Live);
    (meter_level(rms), elapsed_ms, is_live)
  }

  /// Stops the recorder, waits for its last data and puts the take together.
  async fn finish(self, maybe_reason: Option<RecordingStop>) -> Result<(TakeInfo, TakeSource), String> {
    let duration_ms = (now() - self.started_at_ms).max(0.0) as u64;
    let stopped = js_sys::Promise::new(&mut |resolve, _| {
      self.recorder.set_onstop(Some(&resolve));
    });
    if self.recorder.state() != web_sys::RecordingState::Inactive {
      drop(self.recorder.stop());
      drop(JsFuture::from(stopped).await);
    }
    self.recorder.set_onstop(None);
    let chunks = self.chunks.borrow().clone();
    let format = self.format;
    let waveform = Waveform::from_levels(&self.levels);
    self.release();
    let waveform = waveform.ok_or("nothing was recorded")?;
    let parts = js_sys::Array::new();
    for chunk in &chunks {
      parts.push(chunk);
    }
    let bag = web_sys::BlobPropertyBag::new();
    bag.set_type(format.media_type);
    let blob = web_sys::Blob::new_with_blob_sequence_and_options(&parts, &bag)
      .map_err(|_| "the take could not be put together".to_owned())?;
    if blob.size() == 0.0 {
      return Err("nothing was recorded".to_owned());
    }
    Ok((
      TakeInfo {
        duration_ms,
        waveform: waveform.into_bytes(),
        maybe_stop: maybe_reason,
      },
      TakeSource::Browser { blob, format },
    ))
  }

  /// Ends the microphone and the analysis; the browser's recording indicator goes off.
  fn release(&self) {
    if self.recorder.state() != web_sys::RecordingState::Inactive {
      drop(self.recorder.stop());
    }
    self.recorder.set_ondataavailable(None);
    for track in self.stream.get_tracks().iter() {
      if let Ok(track) = track.dyn_into::<web_sys::MediaStreamTrack>() {
        track.stop();
      }
    }
    drop(self.context.close());
  }
}

/// Gives a take to the core: the browser's bytes or file, or the shell's take moved into the
/// upload staging folder. Returns the name, type and source of the attachment.
async fn hand_over(source: TakeSource) -> Result<(String, &'static str, FileSource), String> {
  match source {
    TakeSource::Browser { blob, format } => {
      let file_name = file_name_now(format.extension);
      let source = browser_source(&blob, &file_name, format).await?;
      Ok((file_name, format.media_type, source))
    },
    TakeSource::Shell { path } => {
      let file_name = file_name_now("ogg");
      let args = json!({ "path": path, "file_name": file_name });
      let staged = tauri::call::<String>("voice_take_stage", &args).await?;
      Ok((file_name, "audio/ogg", FileSource::Path { path: staged }))
    },
  }
}

/// A recording dropped anywhere (the app's state disposed at sign-out, a discard) stops its
/// microphone: the browser's recording indicator never stays on for nothing.
impl Drop for BrowserRecording {
  fn drop(&mut self) {
    self.release();
  }
}

/// Opens the microphone in the page and starts recording.
async fn start_in_browser() -> Result<BrowserRecording, RecordingRefusal> {
  let format = browser_format().ok_or(RecordingRefusal::Failed)?;
  let window = web_sys::window().ok_or(RecordingRefusal::Failed)?;
  let devices = window
    .navigator()
    .media_devices()
    .map_err(|_| RecordingRefusal::Microphone)?;
  let constraints = web_sys::MediaStreamConstraints::new();
  let audio = web_sys::MediaTrackConstraints::new();
  audio.set_echo_cancellation(&wasm_bindgen::JsValue::TRUE);
  audio.set_noise_suppression(&wasm_bindgen::JsValue::TRUE);
  audio.set_auto_gain_control(&wasm_bindgen::JsValue::TRUE);
  constraints.set_audio(&audio);
  let promise = devices
    .get_user_media_with_constraints(&constraints)
    .map_err(|_| RecordingRefusal::Microphone)?;
  let stream: web_sys::MediaStream = JsFuture::from(promise)
    .await
    .map_err(|error| {
      let name = js_sys::Reflect::get(&error, &"name".into())
        .ok()
        .and_then(|name| name.as_string())
        .unwrap_or_default();
      tracing::debug!(%name, "the microphone did not open");
      RecordingRefusal::Microphone
    })?
    .unchecked_into();
  let stop_tracks = |stream: &web_sys::MediaStream| {
    for track in stream.get_tracks().iter() {
      if let Ok(track) = track.dyn_into::<web_sys::MediaStreamTrack>() {
        track.stop();
      }
    }
  };
  let options = web_sys::MediaRecorderOptions::new();
  options.set_mime_type(format.mime_type);
  options.set_audio_bits_per_second(BROWSER_BITRATE_BPS);
  let Ok(recorder) =
    web_sys::MediaRecorder::new_with_media_stream_and_media_recorder_options(&stream, &options)
  else {
    stop_tracks(&stream);
    return Err(RecordingRefusal::Failed);
  };
  let Some((context, analyser)) = analyser_of(&stream) else {
    stop_tracks(&stream);
    return Err(RecordingRefusal::Failed);
  };
  let chunks = Rc::new(RefCell::new(Vec::new()));
  let on_data = {
    let chunks = Rc::clone(&chunks);
    Closure::<dyn FnMut(web_sys::BlobEvent)>::new(move |event: web_sys::BlobEvent| {
      if let Some(data) = event.data().filter(|data| data.size() > 0.0) {
        chunks.borrow_mut().push(data);
      }
    })
  };
  recorder.set_ondataavailable(Some(on_data.as_ref().unchecked_ref()));
  // A slice a second: a recording that ends abruptly loses at most the last second.
  if recorder.start_with_time_slice(1000).is_err() {
    stop_tracks(&stream);
    drop(context.close());
    return Err(RecordingRefusal::Failed);
  }
  Ok(BrowserRecording {
    recorder,
    stream,
    context,
    analyser,
    chunks,
    levels: Vec::new(),
    format,
    started_at_ms: now(),
    _on_data: on_data,
  })
}

/// An analyser on the microphone, for the level; not connected to the speakers.
fn analyser_of(stream: &web_sys::MediaStream) -> Option<(web_sys::AudioContext, web_sys::AnalyserNode)> {
  let context = web_sys::AudioContext::new().ok()?;
  let source = context.create_media_stream_source(stream).ok()?;
  let analyser = context.create_analyser().ok()?;
  analyser.set_fft_size(2048);
  source.connect_with_audio_node(&analyser).ok()?;
  Some((context, analyser))
}

async fn start_in_shell(max_seconds: u32) -> Result<(), RecordingRefusal> {
  tauri::call::<()>("voice_record_start", &json!({ "max_seconds": max_seconds }))
    .await
    .map_err(|error| RecordingRefusal::parse(&error))
}

async fn finish_in_shell(maybe_reason: Option<RecordingStop>) -> Result<(TakeInfo, TakeSource), String> {
  let take = tauri::call::<ShellTake>("voice_record_finish", &json!({ "stop": maybe_reason })).await?;
  Ok((
    TakeInfo {
      duration_ms: take.duration_ms,
      waveform: take.waveform,
      maybe_stop: take.maybe_stop,
    },
    TakeSource::Shell { path: take.path },
  ))
}

/// The format this browser records in, the first it supports of [`BROWSER_FORMATS`].
fn browser_format() -> Option<BrowserFormat> {
  let has_recorder = web_sys::window()
    .is_some_and(|window| js_sys::Reflect::has(&window, &"MediaRecorder".into()).unwrap_or(false));
  if !has_recorder {
    return None;
  }
  BROWSER_FORMATS
    .into_iter()
    .find(|format| web_sys::MediaRecorder::is_type_supported(format.mime_type))
}

/// The take as the core takes it: bytes when small, the browser's file otherwise.
async fn browser_source(
  blob: &web_sys::Blob,
  file_name: &str,
  format: BrowserFormat,
) -> Result<FileSource, String> {
  if blob.size() as u64 <= READ_WHOLE_MAX_BYTES {
    let buffer = JsFuture::from(blob.array_buffer())
      .await
      .map_err(|_| "the take could not be read".to_owned())?;
    return Ok(FileSource::Bytes {
      bytes: js_sys::Uint8Array::new(&buffer).to_vec(),
    });
  }
  let parts = js_sys::Array::of1(blob);
  let bag = web_sys::FilePropertyBag::new();
  bag.set_type(format.media_type);
  let file = web_sys::File::new_with_blob_sequence_and_options(&parts, file_name, &bag)
    .map_err(|_| "the take could not be made a file".to_owned())?;
  large_files::source_of(&LargeFile::Picked(file)).await
}

/// The take's name, from the local time now.
fn file_name_now(extension: &str) -> String {
  let date = js_sys::Date::new_0();
  voice::file_name(
    (
      date.get_full_year() as i32,
      date.get_month() + 1,
      date.get_date(),
      date.get_hours(),
      date.get_minutes(),
      date.get_seconds(),
    ),
    extension,
  )
}

/// A meter reading from an RMS level: 0.0 at or below -60 dBFS, linear in decibels.
fn meter_level(rms: f32) -> f32 {
  if !rms.is_finite() || rms <= 0.0 {
    return 0.0;
  }
  ((20.0 * rms.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

fn now() -> f64 {
  web_sys::window()
    .and_then(|window| window.performance())
    .map_or(0.0, |performance| performance.now())
}

/// Whether a call runs or rings: no recording starts and no voice message plays then (#204).
pub fn is_in_call(state: crate::state::AppState) -> bool {
  state.call.phase.get() != teamcraft_client_api::calls::CallPhase::Idle
    || state.call.incoming.with(Option::is_some)
}

/// Stops a recording when a call starts or rings, the page goes to the background or the user
/// leaves the conversation, and pauses voice and audio playback for a call (#204). Ends the
/// recording and deletes the take when the signed-in app goes.
pub fn interrupt_when_needed(state: crate::state::AppState) {
  let recorder = state.voice;
  // Sign-out or another account: the signed-in app goes, and the recording and take with it.
  on_cleanup(move || recorder.release_now());
  Effect::new(move |_| {
    if is_in_call(state) {
      recorder.interrupt(RecordingStop::Call);
      state.audio.pause();
    }
  });
  Effect::new(move |maybe_before: Option<crate::state::MainView>| {
    let view = state.view.get();
    if maybe_before.is_some_and(|before| before != view) {
      recorder.interrupt(RecordingStop::Left);
    }
    view
  });
  let is_visible = crate::reactive::use_page_visible();
  Effect::new(move |_| {
    if !is_visible.get() {
      recorder.interrupt(RecordingStop::Background);
    }
  });
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn meter_follows_decibels() {
    assert_eq!(meter_level(0.0), 0.0);
    assert_eq!(meter_level(f32::NAN), 0.0);
    assert!((meter_level(1.0) - 1.0).abs() < 1e-6);
    assert!((meter_level(0.001) - 0.0).abs() < 1e-6);
    assert!((meter_level(0.031_6) - 0.5).abs() < 0.01);
  }

  #[test]
  fn opus_is_preferred() {
    assert_eq!(BROWSER_FORMATS[0].media_type, "audio/ogg");
    assert_eq!(BROWSER_FORMATS[1].media_type, "audio/webm");
  }
}
