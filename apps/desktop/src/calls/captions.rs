//! Live captions of calls on the desktop (#215, e2ee.md §8.6): the speech model, downloaded on
//! first use into the app's data folder, and the recognizer that runs beside the call.
//!
//! - **Model:** whisper.cpp's multilingual base model (`teamcraft_media::caption_model`), about
//!   57 MiB, downloaded only after the user agreed in the UI, checked against its pinned SHA-256
//!   when written and when loaded. Once there, captions start offline.
//! - **Recognizer:** a worker thread (`teamcraft_media::captions`) with at most
//!   [`MAX_RECOGNIZER_THREADS`] CPU threads for one recognition; the call never waits for it.
//! - **Nothing leaves the device:** the audio is the call's decrypted playback, the text goes to
//!   this webview only, and nobody else in the call is told.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use teamcraft_client_api::calls::{CaptionLanguage, CaptionModelState};
use teamcraft_client_api::failure::{ActionFailure, CallFailure, FailureKind};
use teamcraft_media::caption_model::{CAPTION_MODEL, CaptionModelError};
use teamcraft_media::captions::{CaptionSettings, CaptionTap, CaptionUpdate, CaptionWorker};
use teamcraft_media::captions_whisper::{self, WhisperRecognizer};
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::{CallState, lock};

/// CPU threads one recognition uses at most. whisper.cpp's threads wait busily between steps, so
/// more threads cost more CPU for little less delay (docs/performance.md, "Live captions").
const MAX_RECOGNIZER_THREADS: usize = 2;
/// Per-phase limits of the download (robustness.md, "Hostile networks"): a slow link may take
/// minutes for the whole file, but not this long to connect or between two pieces.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// The UI hears of the download's progress at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// The speech model in the app's data folder.
pub(super) struct CaptionModels {
  folder: PathBuf,
  /// One download at a time; a second caller waits and then finds the model there.
  installing: tokio::sync::Mutex<()>,
}

/// Captions running beside the call: the worker and what it says. Dropping it stops the worker.
pub(super) struct CaptionSession {
  _worker: CaptionWorker,
  updates: mpsc::UnboundedReceiver<CaptionUpdate>,
}

impl CaptionModels {
  pub(super) fn new(folder: PathBuf) -> Self {
    Self {
      folder,
      installing: tokio::sync::Mutex::new(()),
    }
  }

  /// Where the model stands at the app's start (a file check, no hashing).
  pub(super) fn initial_state(&self) -> CaptionModelState {
    if !captions_whisper::is_supported() {
      return CaptionModelState::Unsupported;
    }
    let size_bytes = CAPTION_MODEL.size_bytes;
    if CAPTION_MODEL.is_installed(&self.folder) {
      CaptionModelState::Ready { size_bytes }
    } else {
      CaptionModelState::Missing { size_bytes }
    }
  }

  /// Makes sure the model is there: downloads it if not, telling the UI how far it got through
  /// `state` and `notify`. The error is `ActionFailure` wire text.
  pub(super) async fn ensure(
    &self,
    state: &Arc<Mutex<CallState>>,
    notify: &(dyn Fn() + Send + Sync),
  ) -> Result<(), String> {
    if !captions_whisper::is_supported() {
      return Err(failure(
        CallFailure::CaptionsUnsupported,
        "the processor lacks AVX2, FMA or F16C",
      ));
    }
    let _installing = self.installing.lock().await;
    if CAPTION_MODEL.is_installed(&self.folder) {
      return Ok(());
    }
    info!("downloading the speech model for captions");
    let downloaded = self.download(state, notify).await;
    let size_bytes = CAPTION_MODEL.size_bytes;
    lock(state).maybe_captions_model = Some(match &downloaded {
      Ok(()) => CaptionModelState::Ready { size_bytes },
      Err(_) => CaptionModelState::Missing { size_bytes },
    });
    notify();
    downloaded.map_err(|error| {
      warn!(%error, "the speech model for captions could not be downloaded");
      failure(CallFailure::CaptionModelUnavailable, &error)
    })
  }

  /// Loads the model and starts the recognizer for `language`; the tap goes to the call. Checks
  /// the model's hash first (a quarter of a second), on a blocking thread.
  pub(super) async fn start(
    &self,
    language: CaptionLanguage,
  ) -> Result<(CaptionSession, CaptionTap), String> {
    let folder = self.folder.clone();
    let threads = recognizer_threads();
    let loaded = tokio::task::spawn_blocking(move || {
      let path = CAPTION_MODEL
        .verified_path(&folder)
        .map_err(|error| error.to_string())?;
      WhisperRecognizer::load(&path, threads).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|loaded| loaded);
    let recognizer = loaded.map_err(|error| {
      warn!(%error, "the speech model for captions didn't load");
      failure(CallFailure::CaptionModelUnavailable, &error)
    })?;
    let (sender, updates) = mpsc::unbounded_channel();
    let settings = CaptionSettings {
      language,
      ..CaptionSettings::default()
    };
    // The session may be gone (captions off, the call over) when an update comes.
    let (worker, tap) = CaptionWorker::spawn(Box::new(recognizer), settings, move |update| {
      drop(sender.send(update))
    })
    .map_err(|error| failure(CallFailure::CaptionModelUnavailable, &error.to_string()))?;
    info!(?language, threads, "captions on");
    Ok((
      CaptionSession {
        _worker: worker,
        updates,
      },
      tap,
    ))
  }

  /// Deletes the model (and an unfinished download); captions need a new download after this.
  pub(super) async fn delete(&self) -> Result<(), String> {
    let _installing = self.installing.lock().await;
    let folder = self.folder.clone();
    tokio::task::spawn_blocking(move || CAPTION_MODEL.delete(&folder))
      .await
      .map_err(|error| error.to_string())?
      .map_err(|error| ActionFailure::other(error.to_string()).to_wire())
  }

  async fn download(
    &self,
    state: &Arc<Mutex<CallState>>,
    notify: &(dyn Fn() + Send + Sync),
  ) -> Result<(), String> {
    let client = reqwest::Client::builder()
      .connect_timeout(CONNECT_TIMEOUT)
      .read_timeout(READ_TIMEOUT)
      .build()
      .map_err(|error| error.to_string())?;
    let mut response = client
      .get(CAPTION_MODEL.url)
      .send()
      .await
      .and_then(reqwest::Response::error_for_status)
      .map_err(|error| error.to_string())?;
    let folder = self.folder.clone();
    let mut download = tokio::task::spawn_blocking(move || CAPTION_MODEL.start_download(&folder))
      .await
      .map_err(|error| error.to_string())?
      .map_err(|error| error.to_string())?;
    let size_bytes = CAPTION_MODEL.size_bytes;
    let mut last_progress = std::time::Instant::now();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
      // A buffered write of one network chunk (kilobytes): no need for a blocking thread.
      download.write(&chunk).map_err(|error| error.to_string())?;
      if last_progress.elapsed() >= PROGRESS_INTERVAL {
        last_progress = std::time::Instant::now();
        lock(state).maybe_captions_model = Some(CaptionModelState::Downloading {
          received_bytes: download.written_bytes(),
          size_bytes,
        });
        notify();
      }
    }
    tokio::task::spawn_blocking(move || download.finish())
      .await
      .map_err(|error| error.to_string())?
      .map(drop)
      .map_err(|error: CaptionModelError| error.to_string())
  }
}

impl CaptionSession {
  /// The worker's next update; `None` once it ended (a cancel-safe receive).
  pub(super) async fn next_update(&mut self) -> Option<CaptionUpdate> {
    self.updates.recv().await
  }
}

/// The failure the UI shows when captions stopped by themselves.
pub(super) fn stopped_failure(update: &CaptionUpdate) -> Option<String> {
  match update {
    CaptionUpdate::Text { .. } => None,
    CaptionUpdate::Overloaded => Some(failure(
      CallFailure::CaptionsOverloaded,
      "the recognizer fell behind the speech",
    )),
    CaptionUpdate::Failed => Some(failure(
      CallFailure::CaptionModelUnavailable,
      "the recognizer failed",
    )),
  }
}

fn failure(kind: CallFailure, detail: &str) -> String {
  ActionFailure::new(FailureKind::Call(kind), detail).to_wire()
}

/// Half the cores, at most [`MAX_RECOGNIZER_THREADS`]: the call, the webview and the rest of the
/// computer keep the others.
fn recognizer_threads() -> u16 {
  let cores = std::thread::available_parallelism().map_or(2, std::num::NonZeroUsize::get);
  u16::try_from((cores / 2).clamp(1, MAX_RECOGNIZER_THREADS)).unwrap_or(1)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn recognizer_threads_leave_cores_to_the_rest() {
    let threads = usize::from(recognizer_threads());
    assert!((1..=MAX_RECOGNIZER_THREADS).contains(&threads));
  }

  #[test]
  fn captions_that_stop_by_themselves_say_why() {
    for (update, kind) in [
      (CaptionUpdate::Overloaded, CallFailure::CaptionsOverloaded),
      (CaptionUpdate::Failed, CallFailure::CaptionModelUnavailable),
    ] {
      let wire = stopped_failure(&update).unwrap();
      assert_eq!(ActionFailure::from_wire(&wire).kind, FailureKind::Call(kind));
    }
  }

  #[test]
  fn a_folder_without_the_model_says_missing_with_its_size() {
    let folder = std::env::temp_dir().join(format!("teamcraft-captions-missing-{}", std::process::id()));
    let models = CaptionModels::new(folder);
    match models.initial_state() {
      CaptionModelState::Missing { size_bytes } => assert_eq!(size_bytes, CAPTION_MODEL.size_bytes),
      CaptionModelState::Unsupported => {},
      other => panic!("unexpected {other:?}"),
    }
  }
}
