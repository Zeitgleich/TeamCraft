//! Recording the call on the desktop (issue #216, e2ee.md §8.3). The shell records with
//! `teamcraft-media` into a private folder; the call's loop hands it the audio and sends the signed
//! notice that tells everybody.
//!
//! - The recorder belongs to the host, not to the call: a call that ends finishes the recording
//!   and keeps the take, for the user to post or delete.
//! - A take waits in the private folder until the user posts it (it moves into the core's upload
//!   staging folder, which the core empties once the encrypted upload is done) or deletes it. One
//!   take at a time: a new recording replaces a take nobody posted.
//! - The folder is emptied at every start of the app and at sign-out: a recording a crash left is
//!   not kept (e2ee.md §8.3).
//! - Nothing about the file (name, path) is logged.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use teamcraft_client_api::calls::{CallRecordingTake, OwnCallRecording};
use teamcraft_client_api::failure::ActionFailure;
use teamcraft_media::call_recording::{CallRecorder, CallRecordingError, RecordingSink, clear_recordings};
use teamcraft_proto::{ConversationId, MAX_RECORDING_MINUTES};

use super::{CallState, lock};
use crate::video_playback::create_private_dir;

/// This device's recording and its take, kept with the call state.
#[derive(Default)]
pub(super) struct Recordings {
  maybe_running: Option<Running>,
  maybe_take: Option<Take>,
}

/// The private folder recordings are written to, and how they are numbered.
pub(super) struct RecordingFolder {
  folder: PathBuf,
  next: AtomicU64,
}

struct Running {
  conversation: ConversationId,
  recorder: CallRecorder,
}

struct Take {
  shown: CallRecordingTake,
  path: PathBuf,
}

impl Recordings {
  /// The take the UI offers to post or delete.
  pub(super) fn take(&self) -> Option<CallRecordingTake> {
    self.maybe_take.as_ref().map(|take| take.shown)
  }

  /// Whether the running recording takes nothing more (its maximum, or a failed write).
  pub(super) fn is_ended(&self) -> bool {
    self
      .maybe_running
      .as_ref()
      .is_some_and(|running| running.recorder.is_ended())
  }

  pub(super) fn is_running(&self) -> bool {
    self.maybe_running.is_some()
  }
}

impl RecordingFolder {
  /// `folder` is emptied now: what is there was left by a crash.
  pub(super) fn new(folder: PathBuf) -> Self {
    clear_recordings(&folder);
    Self {
      folder,
      next: AtomicU64::new(0),
    }
  }

  /// Starts recording the active call; returns where its loop hands the audio. `now_ms` is the
  /// wall clock the UI counts from.
  pub(super) fn start(
    &self,
    state: &Mutex<CallState>,
    max_secs: u32,
    now_ms: i64,
  ) -> Result<RecordingSink, String> {
    let max_secs = max_secs.clamp(60, u32::from(MAX_RECORDING_MINUTES) * 60);
    let conversation = {
      let state = lock(state);
      if state.recordings.is_running() {
        return Err("a recording is already running".to_owned());
      }
      state
        .view
        .maybe_active
        .as_ref()
        .map(|active| active.conversation)
        .ok_or_else(|| "no call to record".to_owned())?
    };
    create_private_dir(&self.folder).map_err(|error| error.to_string())?;
    let number = self.next.fetch_add(1, Ordering::Relaxed);
    let path = self
      .folder
      .join(format!("recording-{}-{number}.ogg", std::process::id()));
    let recorder = CallRecorder::start(&path, Duration::from_secs(u64::from(max_secs)))
      .map_err(|error| error.to_string())?;
    let sink = recorder.sink();
    let mut state = lock(state);
    state.recordings.maybe_running = Some(Running {
      conversation,
      recorder,
    });
    if let Some(active) = state.view.maybe_active.as_mut() {
      active.maybe_own_recording = Some(OwnCallRecording {
        started_at: now_ms,
        max_secs,
      });
    }
    Ok(sink)
  }

  /// Deletes the take the user discarded, or every file at sign-out.
  pub(super) fn discard(&self, state: &Mutex<CallState>) {
    if let Some(take) = lock(state).recordings.maybe_take.take() {
      remove(&take.path);
    }
  }

  /// Ends the session's recordings: a running one is cancelled, the take and the folder go.
  pub(super) async fn sign_out(&self, state: &Mutex<CallState>) {
    let maybe_running = lock(state).recordings.maybe_running.take();
    if let Some(running) = maybe_running {
      drop(tokio::task::spawn_blocking(move || running.recorder.cancel()).await);
    }
    self.discard(state);
    clear_recordings(&self.folder);
  }
}

/// Finishes the running recording, if any, and keeps its take (replacing one nobody posted).
/// Returns the failure to show when there is nothing to keep because writing failed.
pub(super) async fn finish(state: &Mutex<CallState>) -> Result<(), String> {
  let maybe_running = {
    let mut state = lock(state);
    if let Some(active) = state.view.maybe_active.as_mut() {
      active.maybe_own_recording = None;
    }
    state.recordings.maybe_running.take()
  };
  let Some(Running {
    conversation,
    recorder,
  }) = maybe_running
  else {
    return Ok(());
  };
  let is_at_limit = recorder.is_at_limit();
  let take = match tokio::task::spawn_blocking(move || recorder.finish()).await {
    Ok(Ok(take)) => take,
    // Stopped before anything was recorded: nothing to keep, nothing went wrong.
    Ok(Err(CallRecordingError::Empty)) => return Ok(()),
    Ok(Err(error)) => {
      tracing::warn!(%error, "the call recording could not be finished");
      return Err(ActionFailure::other(error.to_string()).to_wire());
    },
    Err(error) => {
      tracing::warn!(%error, "finishing the call recording failed");
      return Err(ActionFailure::other(error.to_string()).to_wire());
    },
  };
  let previous = lock(state).recordings.maybe_take.replace(Take {
    shown: CallRecordingTake {
      conversation,
      duration_ms: take.duration_ms,
      size_bytes: take.size_bytes,
      is_at_limit,
    },
    path: take.path,
  });
  if let Some(previous) = previous {
    remove(&previous.path);
  }
  Ok(())
}

/// Moves the take into `staging` for the core to upload, under a name the shell picks; returns
/// the path the message names. The take is no longer offered.
pub(super) fn stage(state: &Mutex<CallState>, staging: &Path) -> Result<PathBuf, String> {
  let take = lock(state)
    .recordings
    .maybe_take
    .take()
    .ok_or_else(|| "no recording to post".to_owned())?;
  create_private_dir(staging).map_err(|error| error.to_string())?;
  let file_name = take
    .path
    .file_name()
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_else(|| "recording.ogg".to_owned());
  let target = staging.join(format!("call-{file_name}"));
  // Across file systems a rename fails: copy, then delete.
  if fs::rename(&take.path, &target).is_err() {
    if let Err(error) = fs::copy(&take.path, &target) {
      // The take stays offered: the user can try again or delete it.
      lock(state).recordings.maybe_take = Some(take);
      return Err(error.to_string());
    }
    remove(&take.path);
  }
  Ok(target)
}

fn remove(path: &Path) {
  if let Err(error) = fs::remove_file(path)
    && error.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!(%error, "deleting a call recording failed");
  }
}
