//! Recording the call on the desktop (issue #216, e2ee.md §8.3). The shell records with
//! `teamcraft-media` into a private folder; the call's loop hands it the audio and sends the signed
//! notice that tells everybody.
//!
//! - The recorder belongs to the host, not to the call: a call that ends finishes the recording
//!   and keeps the take, for the user to post or delete.
//! - The file is encrypted on disk while it is recorded and while it waits (issue #2129), under a
//!   key only this process holds (`teamcraft-media::call_recording`). A take waits in the private
//!   folder until the user posts it (it is decrypted into the core's upload staging folder, which
//!   the core empties once the encrypted upload is done, and the sealed file is deleted) or
//!   deletes it. One take at a time: a new recording replaces a take nobody posted.
//! - The folder is emptied at every start of the app and at sign-out: a recording a crash left is
//!   not kept, and couldn't be read anyway, since its key died with the process (e2ee.md §8.3).
//! - Nothing about the file (name, path) is logged.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use teamcraft_client_api::calls::{CallRecordingTake, OwnCallRecording};
use teamcraft_client_api::failure::ActionFailure;
use teamcraft_media::call_recording::{
  CallRecorder, CallRecordingError, CallRecordingTake as CallRecordingTakeFile, RecordingSink,
  clear_recordings,
};
use teamcraft_proto::{ConversationId, MAX_RECORDING_MINUTES};

use super::{CallState, lock};
use crate::video_playback::create_private_dir;

/// Names tried in the upload folder before posting gives up.
const MAX_STAGING_NAMES: u32 = 100;

/// This device's recording and its take, kept with the call state.
#[derive(Default)]
pub(super) struct Recordings {
  maybe_running: Option<Running>,
  maybe_take: Option<Take>,
  /// Counts sign-outs: a take being posted while one happens leaves nothing behind.
  session: u64,
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
  /// The sealed file and its key.
  recorded: CallRecordingTakeFile,
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
    let path = self.folder.join(format!("recording-{number}.ogg"));
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
        is_unconfirmed: false,
      });
    }
    Ok(sink)
  }

  /// Deletes the take the user discarded, or every file at sign-out.
  pub(super) fn discard(&self, state: &Mutex<CallState>) {
    if let Some(take) = lock(state).recordings.maybe_take.take() {
      remove(&take.recorded.path);
    }
  }

  /// Ends the session's recordings: a running one is cancelled, the take and the folder go.
  pub(super) async fn sign_out(&self, state: &Mutex<CallState>) {
    let maybe_running = {
      let mut state = lock(state);
      state.recordings.session += 1;
      state.recordings.maybe_running.take()
    };
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
      is_at_memory_limit: false,
    },
    recorded: take,
  });
  if let Some(previous) = previous {
    remove(&previous.recorded.path);
  }
  Ok(())
}

/// Decrypts the take into `staging` for the core to upload, under a name the shell picks, and
/// deletes the sealed file (its key goes with the take); returns the path the message names. The
/// take is no longer offered. On a failure it stays offered: the user can try again or delete it.
/// A sign-out while it runs leaves nothing behind: no copy, no take.
pub(super) async fn stage(state: &Mutex<CallState>, staging: &Path) -> Result<PathBuf, String> {
  let (take, session) = {
    let mut state = lock(state);
    let take = state
      .recordings
      .maybe_take
      .take()
      .ok_or_else(|| "no recording to post".to_owned())?;
    (take, state.recordings.session)
  };
  let staging = staging.to_path_buf();
  let sealed_path = take.recorded.path.clone();
  // The copy being written, so a panic doesn't leave it behind.
  let claimed = Arc::new(Mutex::new(None));
  let task_claimed = Arc::clone(&claimed);
  // Up to a few hundred MB to decrypt and write: off the async runtime.
  let joined = tokio::task::spawn_blocking(move || {
    let result = decrypt_to_staging(&take.recorded, &staging, &task_claimed);
    (take, result)
  })
  .await;
  match joined {
    Ok((take, result)) => settle_stage(&mut lock(state).recordings, session, take, result),
    // The task panicked and took the take (and its key) with it: nothing left to offer, and the
    // sealed file can't be read any more.
    Err(error) => {
      remove(&sealed_path);
      if let Some(target) = lock(&claimed).take() {
        remove(&target);
      }
      Err(ActionFailure::other(error.to_string()).to_wire())
    },
  }
}

/// What becomes of the take once its copy for the upload is written (or failed), with the
/// recordings of now.
fn settle_stage(
  recordings: &mut Recordings,
  session: u64,
  take: Take,
  result: Result<PathBuf, String>,
) -> Result<PathBuf, String> {
  if recordings.session != session {
    // Signed out meanwhile: the copy and the take go, as sign-out would have deleted them.
    if let Ok(target) = &result {
      remove(target);
    }
    remove(&take.recorded.path);
    return Err(ActionFailure::other("signed out while the recording was prepared for posting").to_wire());
  }
  match result {
    Ok(target) => {
      remove(&take.recorded.path);
      Ok(target)
    },
    Err(error) => {
      tracing::warn!(%error, "the call recording could not be prepared for posting");
      if recordings.maybe_take.is_none() {
        recordings.maybe_take = Some(take);
      } else {
        // A newer take finished meanwhile and is the one offered; this one goes.
        remove(&take.recorded.path);
      }
      Err(ActionFailure::other(error).to_wire())
    },
  }
}

/// Writes the decrypted take into `staging` under a fresh name; `claimed` names the file while it
/// is written. A failed copy is removed.
fn decrypt_to_staging(
  recorded: &CallRecordingTakeFile,
  staging: &Path,
  claimed: &Mutex<Option<PathBuf>>,
) -> Result<PathBuf, String> {
  create_private_dir(staging).map_err(|error| error.to_string())?;
  let (target, file) = claim_staging_file(recorded, staging)?;
  *lock(claimed) = Some(target.clone());
  let mut output = BufWriter::new(file);
  let result = recorded
    .decrypt_to(&mut output)
    .and_then(|_| output.flush().map_err(CallRecordingError::File));
  *lock(claimed) = None;
  match result {
    Ok(()) => Ok(target),
    Err(error) => {
      drop(output);
      remove(&target);
      Err(error.to_string())
    },
  }
}

/// Creates a new file in `staging` for the take's copy. A copy of an earlier take may still wait
/// for its upload under the same name (numbers start anew with the app): it is never replaced.
fn claim_staging_file(recorded: &CallRecordingTakeFile, staging: &Path) -> Result<(PathBuf, File), String> {
  let stem = recorded
    .path
    .file_stem()
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_else(|| "recording".to_owned());
  for attempt in 1..=MAX_STAGING_NAMES {
    let target = if attempt == 1 {
      staging.join(format!("call-{stem}.ogg"))
    } else {
      staging.join(format!("call-{stem}-{attempt}.ogg"))
    };
    match OpenOptions::new().write(true).create_new(true).open(&target) {
      Ok(file) => return Ok((target, file)),
      Err(error) if error.kind() == ErrorKind::AlreadyExists => {},
      Err(error) => return Err(error.to_string()),
    }
  }
  Err("no free name in the upload folder".to_owned())
}

fn remove(path: &Path) {
  if let Err(error) = fs::remove_file(path)
    && error.kind() != ErrorKind::NotFound
  {
    tracing::warn!(%error, "deleting a call recording failed");
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn a_sign_out_while_posting_leaves_neither_copy_nor_take() {
    let folder = folder("sign-out");
    let (take, copy) = (sealed_take(&folder, "a"), written(&folder, "copy.ogg"));
    let sealed = take.recorded.path.clone();
    let mut recordings = Recordings::default();
    let session = recordings.session;
    recordings.session += 1;
    assert!(settle_stage(&mut recordings, session, take, Ok(copy.clone())).is_err());
    assert!(!copy.exists() && !sealed.exists());
    assert!(recordings.maybe_take.is_none());
    // A failed copy after a sign-out isn't offered again either.
    let take = sealed_take(&folder, "b");
    let sealed = take.recorded.path.clone();
    assert!(settle_stage(&mut recordings, session, take, Err("disk full".to_owned())).is_err());
    assert!(!sealed.exists());
    assert!(recordings.maybe_take.is_none());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn a_posted_take_leaves_only_its_copy() {
    let folder = folder("posted");
    let (take, copy) = (sealed_take(&folder, "a"), written(&folder, "copy.ogg"));
    let sealed = take.recorded.path.clone();
    let mut recordings = Recordings::default();
    let session = recordings.session;
    assert_eq!(
      settle_stage(&mut recordings, session, take, Ok(copy.clone())),
      Ok(copy.clone())
    );
    assert!(copy.exists() && !sealed.exists());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn a_failed_post_is_offered_again_unless_a_newer_take_is_there() {
    let folder = folder("failed");
    let mut recordings = Recordings::default();
    let session = recordings.session;
    let first = sealed_take(&folder, "a");
    let first_path = first.recorded.path.clone();
    assert!(settle_stage(&mut recordings, session, first, Err("disk full".to_owned())).is_err());
    assert_eq!(
      recordings
        .maybe_take
        .as_ref()
        .map(|take| take.recorded.path.clone()),
      Some(first_path.clone())
    );
    // A newer take finished while the next attempt ran: the newer one stays offered.
    let retried = recordings.maybe_take.take().unwrap();
    let newer = sealed_take(&folder, "b");
    let newer_path = newer.recorded.path.clone();
    recordings.maybe_take = Some(newer);
    assert!(settle_stage(&mut recordings, session, retried, Err("disk full".to_owned())).is_err());
    assert_eq!(
      recordings
        .maybe_take
        .as_ref()
        .map(|take| take.recorded.path.clone()),
      Some(newer_path.clone())
    );
    assert!(!first_path.exists() && newer_path.exists());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn the_copy_never_replaces_a_waiting_one() {
    let folder = folder("names");
    let take = sealed_take(&folder, "a");
    let staging = folder.join("staging");
    create_private_dir(&staging).unwrap();
    let claimed = Mutex::new(None);
    let first = decrypt_to_staging(&take.recorded, &staging, &claimed).unwrap();
    let second = decrypt_to_staging(&take.recorded, &staging, &claimed).unwrap();
    assert_ne!(first, second);
    assert_eq!(&fs::read(&first).unwrap()[..4], b"OggS");
    assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
    assert!(lock(&claimed).is_none());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn a_copy_that_fails_to_decrypt_is_removed() {
    let folder = folder("tampered");
    let take = sealed_take(&folder, "a");
    let mut sealed = fs::read(&take.recorded.path).unwrap();
    let middle = sealed.len() / 2;
    sealed[middle] ^= 1;
    fs::write(&take.recorded.path, &sealed).unwrap();
    let staging = folder.join("staging");
    assert!(decrypt_to_staging(&take.recorded, &staging, &Mutex::new(None)).is_err());
    assert_eq!(fs::read_dir(&staging).unwrap().count(), 0);
    fs::remove_dir_all(folder).unwrap();
  }

  /// A finished take of one second of silence, sealed in `folder`.
  fn sealed_take(folder: &Path, name: &str) -> Take {
    let recorder = CallRecorder::start(&folder.join(format!("{name}.ogg")), Duration::from_secs(60)).unwrap();
    for _ in 0..50 {
      recorder.sink().push_playback(&[0.0; 960]);
    }
    let recorded = recorder.finish().unwrap();
    Take {
      shown: CallRecordingTake {
        conversation: ConversationId::from_snowflake(Snowflake::new(1).unwrap()),
        duration_ms: recorded.duration_ms,
        size_bytes: recorded.size_bytes,
        is_at_limit: false,
        is_at_memory_limit: false,
      },
      recorded,
    }
  }

  fn written(folder: &Path, name: &str) -> PathBuf {
    let path = folder.join(name);
    fs::write(&path, b"copy").unwrap();
    path
  }

  fn folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!(
      "teamcraft-desktop-recording-{name}-{}",
      std::process::id()
    ));
    drop(fs::remove_dir_all(&folder));
    fs::create_dir_all(&folder).unwrap();
    folder
  }
}
