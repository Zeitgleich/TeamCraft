//! Recording the call on the desktop (issue #216, e2ee.md §8.3). The shell records with
//! `teamcraft-media` into a private folder; the call's loop hands it the audio and sends the signed
//! notice that tells everybody.
//!
//! - The recorder belongs to the host, not to the call: a call that ends finishes the recording
//!   and keeps the take, for the user to post, save or delete.
//! - The file is encrypted on disk while it is recorded and while it waits (issue #2129). A take
//!   waits in the private folder until the user posts it (it is decrypted into the core's upload
//!   staging folder, which the core empties once the encrypted upload is done, and the sealed file
//!   is deleted) or deletes it. "Save as file…" (#2116) decrypts a copy to where the user picks,
//!   unencrypted, if the verified policy allows it; the take stays. One take at a time: a new
//!   recording replaces a take nobody posted.
//! - **Crash or quit (#2116):** the recording's key is kept next to the file, sealed under the
//!   recording key of the local secrets (`SecretVault::recording_key`), with the conversation and
//!   the start time. The next start offers what a crash, a kill or a quit left, one after another,
//!   for the user to post, save or delete (an unfinished one up to its last whole chunk). A file
//!   whose key wasn't kept (the secrets couldn't be read when it started) is deleted, as it can't
//!   be read. Without kept keys the start doesn't touch the local secrets at all.
//! - Sign-out deletes the folder and forgets the recording key, so a kept key a deletion missed
//!   doesn't open any more.
//! - Nothing about the file (name, path) is logged.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use teamcraft_client_api::calls::{CallRecordingTake, LeftOverRecording, OwnCallRecording};
use teamcraft_client_api::failure::ActionFailure;
use teamcraft_core::store::SecretVault;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_media::call_recording::{
  CallRecorder, CallRecordingError, CallRecordingTake as CallRecordingTakeFile, FolderAtStart, KeptKey,
  LeftoverRecording, RecordingSink, clear_recordings, delete_recording,
};
use teamcraft_proto::{ConversationId, MAX_RECORDING_MINUTES};

use super::{CallState, lock};
use crate::video_playback::create_private_dir;

/// Names tried in the upload folder before posting gives up.
const MAX_STAGING_NAMES: u32 = 100;

/// Where the recording key of the local secrets comes from (the [`SecretVault`]; a fake in tests).
pub trait RecordingKeys: Send + Sync {
  /// The key, made on first use.
  fn key(&self) -> Result<SymmetricKey, String>;
  /// The key if there is one; `Ok(None)` when none was made or it was forgotten.
  fn existing_key(&self) -> Result<Option<SymmetricKey>, String>;
  /// Forgets the key (sign-out).
  fn forget(&self) -> Result<(), String>;
}

/// This device's recording and its takes, kept with the call state.
#[derive(Default)]
pub(super) struct Recordings {
  maybe_running: Option<Running>,
  maybe_take: Option<Take>,
  /// The take while a copy of it is written (posting, saving): still shown, out of reach of the
  /// other actions.
  maybe_copying: Option<CallRecordingTake>,
  /// Takes the last run left (#2116), oldest first: each is offered once the one before is posted
  /// or deleted.
  waiting: Vec<Take>,
  /// Counts sign-outs: a take being posted while one happens leaves nothing behind.
  session: u64,
}

/// The private folder recordings are written to, and how they are numbered.
pub(super) struct RecordingFolder {
  folder: PathBuf,
  next: AtomicU64,
  /// The recording key of the local secrets, when the app has them.
  maybe_keys: Option<Arc<dyn RecordingKeys>>,
  /// What the folder held at start, until it is recovered or cleared.
  at_start: Mutex<Option<FolderAtStart>>,
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

/// What is kept with a recording's key so the next start can offer it again (#2116); sealed with
/// the key.
#[derive(Serialize, Deserialize)]
struct RecordingNote {
  conversation: ConversationId,
  started_at: i64,
}

impl Recordings {
  /// The take the UI offers to post, save or delete.
  pub(super) fn take(&self) -> Option<CallRecordingTake> {
    self
      .maybe_take
      .as_ref()
      .map(|take| take.shown)
      .or(self.maybe_copying)
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

  /// Offers the next waiting take once none is offered.
  fn offer_next(&mut self) {
    if self.maybe_take.is_none() && self.maybe_copying.is_none() && !self.waiting.is_empty() {
      self.maybe_take = Some(self.waiting.remove(0));
    }
  }

  /// Ends every take: the files and their kept keys go.
  fn drop_takes(&mut self) {
    self.maybe_copying = None;
    for take in self.maybe_take.take().into_iter().chain(self.waiting.drain(..)) {
      delete_recording(&take.recorded.path);
    }
  }
}

impl RecordingFolder {
  /// Lists what `folder` holds now: what the last run left. Without kept keys nothing of it can be
  /// read, and it is deleted right away; otherwise [`Self::recover`] looks at it.
  pub(super) fn new(folder: PathBuf, maybe_keys: Option<Arc<dyn RecordingKeys>>) -> Self {
    let at_start = FolderAtStart::list(&folder);
    let maybe_at_start = if at_start.has_kept_keys() && maybe_keys.is_some() {
      Some(at_start)
    } else {
      at_start.clear();
      None
    };
    Self {
      folder,
      next: AtomicU64::new(0),
      maybe_keys,
      at_start: Mutex::new(maybe_at_start),
    }
  }

  /// Offers what the last run left (#2116): opens the kept keys with the recording key and reads
  /// each recording once. Answers whether anything is offered now. When the local secrets can't be
  /// read (a locked keychain), the files stay for the next start; when there is no recording key
  /// (it went with a sign-out), they can't be read and are deleted.
  pub(super) async fn recover(&self, state: &Mutex<CallState>) -> bool {
    let (Some(at_start), Some(keys)) = (lock(&self.at_start).take(), self.maybe_keys.clone()) else {
      return false;
    };
    let session = lock(state).recordings.session;
    let recovered = tokio::task::spawn_blocking(move || match keys.existing_key() {
      Ok(Some(device_key)) => at_start.recover(&device_key),
      Ok(None) => {
        at_start.clear();
        Vec::new()
      },
      Err(error) => {
        tracing::warn!(%error, "call recordings left at the last run stay for the next start");
        Vec::new()
      },
    })
    .await
    .unwrap_or_default();
    let mut takes: Vec<Take> = recovered.into_iter().filter_map(left_over_take).collect();
    takes.sort_by_key(|take| take.shown.maybe_left_over.map(|left_over| left_over.started_at));
    if takes.is_empty() {
      return false;
    }
    let mut state = lock(state);
    if state.recordings.session != session {
      // Signed out meanwhile: what the last run left goes with the session.
      for take in takes {
        delete_recording(&take.recorded.path);
      }
      return false;
    }
    state.recordings.waiting.append(&mut takes);
    state.recordings.offer_next();
    true
  }

  /// Starts recording the active call; returns where its loop hands the audio. `now_ms` is the
  /// wall clock the UI counts from. The recording's key is kept for a crash when the local
  /// secrets give the recording key; without it the recording runs all the same.
  pub(super) async fn start(
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
    let maybe_kept = self.kept_key(conversation, now_ms).await;
    create_private_dir(&self.folder).map_err(|error| error.to_string())?;
    let number = self.next.fetch_add(1, Ordering::Relaxed);
    // The start time keeps the name apart from what the last run left in the folder.
    let path = self.folder.join(format!("recording-{now_ms}-{number}.ogg"));
    let recorder = CallRecorder::start(&path, Duration::from_secs(u64::from(max_secs)), maybe_kept)
      .map_err(|error| error.to_string())?;
    let sink = recorder.sink();
    let mut state = lock(state);
    if state.recordings.is_running() {
      recorder.cancel();
      return Err("a recording is already running".to_owned());
    }
    state.recordings.maybe_running = Some(Running {
      conversation,
      recorder,
    });
    if let Some(active) = state.view.maybe_active.as_mut() {
      active.maybe_own_recording = Some(OwnCallRecording {
        started_at: now_ms,
        max_secs,
        is_unconfirmed: false,
        maybe_consent: None,
      });
    }
    Ok(sink)
  }

  /// Deletes the take the user discarded; the next one waiting is offered.
  pub(super) fn discard(&self, state: &Mutex<CallState>) {
    let mut state = lock(state);
    if let Some(take) = state.recordings.maybe_take.take() {
      delete_recording(&take.recorded.path);
    }
    state.recordings.offer_next();
  }

  /// Ends the session's recordings: a running one is cancelled, the takes and the folder go, and
  /// the recording key is forgotten.
  pub(super) async fn sign_out(&self, state: &Mutex<CallState>) {
    let maybe_running = {
      let mut state = lock(state);
      state.recordings.session += 1;
      state.recordings.maybe_running.take()
    };
    if let Some(running) = maybe_running {
      drop(tokio::task::spawn_blocking(move || running.recorder.cancel()).await);
    }
    lock(state).recordings.drop_takes();
    drop(lock(&self.at_start).take());
    clear_recordings(&self.folder);
    if let Some(keys) = self.maybe_keys.clone() {
      let forgotten = tokio::task::spawn_blocking(move || keys.forget()).await;
      if let Ok(Err(error)) = forgotten {
        tracing::warn!(%error, "the call recording key could not be forgotten at sign-out");
      }
    }
  }

  /// The recording key and the note to keep with this recording's key; `None` (logged) when the
  /// local secrets can't give it.
  async fn kept_key(&self, conversation: ConversationId, started_at: i64) -> Option<KeptKey> {
    let keys = self.maybe_keys.clone()?;
    let note = serde_json::to_vec(&RecordingNote {
      conversation,
      started_at,
    })
    .ok()?;
    match tokio::task::spawn_blocking(move || keys.key()).await {
      Ok(Ok(device_key)) => Some(KeptKey { device_key, note }),
      Ok(Err(error)) => {
        tracing::warn!(%error, "no recording key: a crash would lose this call recording");
        None
      },
      Err(error) => {
        tracing::warn!(%error, "no recording key: a crash would lose this call recording");
        None
      },
    }
  }
}

impl RecordingKeys for SecretVault {
  fn key(&self) -> Result<SymmetricKey, String> {
    self.recording_key().map_err(|error| error.to_string())
  }

  fn existing_key(&self) -> Result<Option<SymmetricKey>, String> {
    self.existing_recording_key().map_err(|error| error.to_string())
  }

  fn forget(&self) -> Result<(), String> {
    self.forget_recording_key().map_err(|error| error.to_string())
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
      maybe_left_over: None,
    },
    recorded: take,
  });
  if let Some(previous) = previous {
    delete_recording(&previous.recorded.path);
  }
  Ok(())
}

/// Decrypts the take into `staging` for the core to upload, under a name the shell picks, and
/// deletes the sealed file (its key goes with the take); returns the path the message names. The
/// take is no longer offered (the next waiting one is). On a failure it stays offered: the user
/// can try again or delete it. A sign-out while it runs leaves nothing behind: no copy, no take.
pub(super) async fn stage(state: &Mutex<CallState>, staging: &Path) -> Result<PathBuf, String> {
  let staging = staging.to_path_buf();
  copy_take(state, move |recorded, claimed| {
    decrypt_to_staging(recorded, &staging, claimed)
  })
  .await
  .map(|(target, _)| target)
}

/// Decrypts a copy of the take to `target`, a place the user picked (#2116): unencrypted, as the
/// page warned. It is written next to `target` first and renamed over it once whole, so a full
/// disk or a take that doesn't decrypt leaves no half file and doesn't destroy a file the user
/// chose to replace. The take stays offered, for posting or deleting.
pub(super) async fn save(state: &Mutex<CallState>, target: PathBuf) -> Result<(), String> {
  copy_take(state, move |recorded, claimed| {
    decrypt_to_file(recorded, &target, claimed)
  })
  .await
  .map(|_| ())
}

/// Takes the offered take out while `copy` writes a copy of it (off the async runtime: up to a
/// few hundred MB to decrypt and write), then settles: a staged take goes, a saved one is offered
/// again. `claimed` names the copy while it is written, so a panic doesn't leave it behind.
async fn copy_take<F>(state: &Mutex<CallState>, copy: F) -> Result<(PathBuf, CopyKind), String>
where
  F: FnOnce(&CallRecordingTakeFile, &Mutex<Option<PathBuf>>) -> Result<(PathBuf, CopyKind), String>
    + Send
    + 'static,
{
  let (take, session) = {
    let mut state = lock(state);
    let take = state
      .recordings
      .maybe_take
      .take()
      .ok_or_else(|| "no recording to copy".to_owned())?;
    state.recordings.maybe_copying = Some(take.shown);
    (take, state.recordings.session)
  };
  let sealed_path = take.recorded.path.clone();
  let claimed = Arc::new(Mutex::new(None));
  let task_claimed = Arc::clone(&claimed);
  let joined = tokio::task::spawn_blocking(move || {
    let result = copy(&take.recorded, &task_claimed);
    (take, result)
  })
  .await;
  match joined {
    Ok((take, result)) => {
      let mut state = lock(state);
      let settled = settle_copy(&mut state.recordings, session, take, result);
      state.recordings.offer_next();
      settled
    },
    // The task panicked and took the take (and its key) with it: nothing left to offer, and the
    // sealed file can't be read any more.
    Err(error) => {
      delete_recording(&sealed_path);
      if let Some(target) = lock(&claimed).take() {
        remove(&target);
      }
      let mut state = lock(state);
      state.recordings.maybe_copying = None;
      state.recordings.offer_next();
      Err(ActionFailure::other(error.to_string()).to_wire())
    },
  }
}

/// What a copy of a take was for: posting (the take goes once it is written) or the user's own
/// file (the take stays).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopyKind {
  Staged,
  Saved,
}

/// What becomes of the take once its copy is written (or failed), with the recordings of now.
fn settle_copy(
  recordings: &mut Recordings,
  session: u64,
  take: Take,
  result: Result<(PathBuf, CopyKind), String>,
) -> Result<(PathBuf, CopyKind), String> {
  recordings.maybe_copying = None;
  if recordings.session != session {
    // Signed out meanwhile: a staged copy and the take go, as sign-out would have deleted them. A
    // copy the user saved is theirs.
    if let Ok((target, CopyKind::Staged)) = &result {
      remove(target);
    }
    delete_recording(&take.recorded.path);
    return Err(ActionFailure::other("signed out while the recording was copied").to_wire());
  }
  match result {
    Ok((target, CopyKind::Staged)) => {
      delete_recording(&take.recorded.path);
      Ok((target, CopyKind::Staged))
    },
    Ok(saved) => {
      keep_offering(recordings, take);
      Ok(saved)
    },
    Err(error) => {
      tracing::warn!(%error, "the call recording could not be copied");
      keep_offering(recordings, take);
      Err(ActionFailure::other(error).to_wire())
    },
  }
}

/// Offers the take again, unless a newer take finished meanwhile and is the one offered: then this
/// one goes.
fn keep_offering(recordings: &mut Recordings, take: Take) {
  if recordings.maybe_take.is_none() {
    recordings.maybe_take = Some(take);
  } else {
    delete_recording(&take.recorded.path);
  }
}

/// A take of what the last run left, from its note; `None` (and deleted) when the note doesn't
/// read, as nothing could post it.
fn left_over_take(leftover: LeftoverRecording) -> Option<Take> {
  let LeftoverRecording { take, note, .. } = leftover;
  let Ok(note) = serde_json::from_slice::<RecordingNote>(note.expose_secret()) else {
    delete_recording(&take.path);
    return None;
  };
  Some(Take {
    shown: CallRecordingTake {
      conversation: note.conversation,
      duration_ms: take.duration_ms,
      size_bytes: take.size_bytes,
      is_at_limit: false,
      is_at_memory_limit: false,
      maybe_left_over: Some(LeftOverRecording {
        started_at: note.started_at,
        is_unfinished: take.is_unfinished(),
      }),
    },
    recorded: take,
  })
}

/// Writes the decrypted take into `staging` under a fresh name; `claimed` names the file while it
/// is written. A failed copy is removed.
fn decrypt_to_staging(
  recorded: &CallRecordingTakeFile,
  staging: &Path,
  claimed: &Mutex<Option<PathBuf>>,
) -> Result<(PathBuf, CopyKind), String> {
  create_private_dir(staging).map_err(|error| error.to_string())?;
  let (target, file) = claim_staging_file(recorded, staging)?;
  write_decrypted(recorded, &target, file, claimed)?;
  Ok((target, CopyKind::Staged))
}

/// Writes the decrypted take to a new file next to `target` and renames it over `target` once it
/// is whole; a failed copy is removed and `target` left as it was.
fn decrypt_to_file(
  recorded: &CallRecordingTakeFile,
  target: &Path,
  claimed: &Mutex<Option<PathBuf>>,
) -> Result<(PathBuf, CopyKind), String> {
  let name = target
    .file_name()
    .ok_or_else(|| "the picked place is not a file".to_owned())?
    .to_string_lossy()
    .into_owned();
  let partial = target.with_file_name(format!(".{name}.partial"));
  let file = OpenOptions::new()
    .write(true)
    .create(true)
    .truncate(true)
    .open(&partial)
    .map_err(|error| error.to_string())?;
  write_decrypted(recorded, &partial, file, claimed)?;
  if let Err(error) = fs::rename(&partial, target) {
    remove(&partial);
    return Err(error.to_string());
  }
  Ok((target.to_path_buf(), CopyKind::Saved))
}

/// Decrypts the take into `file` at `path` and syncs it; on a failure (a full disk, a take that
/// doesn't decrypt) the file is removed.
fn write_decrypted(
  recorded: &CallRecordingTakeFile,
  path: &Path,
  file: File,
  claimed: &Mutex<Option<PathBuf>>,
) -> Result<(), String> {
  *lock(claimed) = Some(path.to_path_buf());
  let mut output = BufWriter::new(file);
  let result = recorded
    .decrypt_to(&mut output)
    .and_then(|_| output.flush().map_err(CallRecordingError::File))
    .and_then(|()| output.get_ref().sync_all().map_err(CallRecordingError::File));
  *lock(claimed) = None;
  if let Err(error) = result {
    drop(output);
    remove(path);
    return Err(error.to_string());
  }
  Ok(())
}

/// Creates a new file in `staging` for the take's copy. A copy of an earlier take may still wait
/// for its upload under the same name: it is never replaced.
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
    let (take, copy) = (sealed_take(&folder, "a", None), written(&folder, "copy.ogg"));
    let sealed = take.recorded.path.clone();
    let mut recordings = Recordings::default();
    let session = recordings.session;
    recordings.session += 1;
    assert!(
      settle_copy(
        &mut recordings,
        session,
        take,
        Ok((copy.clone(), CopyKind::Staged))
      )
      .is_err()
    );
    assert!(!copy.exists() && !sealed.exists());
    assert!(recordings.maybe_take.is_none());
    // A failed copy after a sign-out isn't offered again either.
    let take = sealed_take(&folder, "b", None);
    let sealed = take.recorded.path.clone();
    assert!(settle_copy(&mut recordings, session, take, Err("disk full".to_owned())).is_err());
    assert!(!sealed.exists());
    assert!(recordings.maybe_take.is_none());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn a_posted_take_leaves_only_its_copy() {
    let folder = folder("posted");
    let (take, copy) = (sealed_take(&folder, "a", None), written(&folder, "copy.ogg"));
    let sealed = take.recorded.path.clone();
    let mut recordings = Recordings::default();
    let session = recordings.session;
    assert_eq!(
      settle_copy(
        &mut recordings,
        session,
        take,
        Ok((copy.clone(), CopyKind::Staged))
      ),
      Ok((copy.clone(), CopyKind::Staged))
    );
    assert!(copy.exists() && !sealed.exists());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn a_saved_take_stays_offered_and_a_sign_out_keeps_the_saved_file() {
    let folder = folder("saved");
    let (take, copy) = (sealed_take(&folder, "a", None), written(&folder, "saved.ogg"));
    let sealed = take.recorded.path.clone();
    let mut recordings = Recordings::default();
    let session = recordings.session;
    assert!(
      settle_copy(
        &mut recordings,
        session,
        take,
        Ok((copy.clone(), CopyKind::Saved))
      )
      .is_ok()
    );
    assert!(copy.exists() && sealed.exists());
    assert!(recordings.maybe_take.is_some());
    let take = recordings.maybe_take.take().unwrap();
    recordings.session += 1;
    assert!(
      settle_copy(
        &mut recordings,
        session,
        take,
        Ok((copy.clone(), CopyKind::Saved))
      )
      .is_err()
    );
    assert!(copy.exists(), "the user's own file");
    assert!(!sealed.exists());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn a_failed_post_is_offered_again_unless_a_newer_take_is_there() {
    let folder = folder("failed");
    let mut recordings = Recordings::default();
    let session = recordings.session;
    let first = sealed_take(&folder, "a", None);
    let first_path = first.recorded.path.clone();
    assert!(settle_copy(&mut recordings, session, first, Err("disk full".to_owned())).is_err());
    assert_eq!(
      recordings
        .maybe_take
        .as_ref()
        .map(|take| take.recorded.path.clone()),
      Some(first_path.clone())
    );
    // A newer take finished while the next attempt ran: the newer one stays offered.
    let retried = recordings.maybe_take.take().unwrap();
    let newer = sealed_take(&folder, "b", None);
    let newer_path = newer.recorded.path.clone();
    recordings.maybe_take = Some(newer);
    assert!(settle_copy(&mut recordings, session, retried, Err("disk full".to_owned())).is_err());
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
    let take = sealed_take(&folder, "a", None);
    let staging = folder.join("staging");
    create_private_dir(&staging).unwrap();
    let claimed = Mutex::new(None);
    let (first, _) = decrypt_to_staging(&take.recorded, &staging, &claimed).unwrap();
    let (second, _) = decrypt_to_staging(&take.recorded, &staging, &claimed).unwrap();
    assert_ne!(first, second);
    assert_eq!(&fs::read(&first).unwrap()[..4], b"OggS");
    assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
    assert!(lock(&claimed).is_none());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn a_copy_that_fails_to_decrypt_is_removed() {
    let folder = folder("tampered");
    let take = sealed_take(&folder, "a", None);
    let mut sealed = fs::read(&take.recorded.path).unwrap();
    let middle = sealed.len() / 2;
    sealed[middle] ^= 1;
    fs::write(&take.recorded.path, &sealed).unwrap();
    let staging = folder.join("staging");
    assert!(decrypt_to_staging(&take.recorded, &staging, &Mutex::new(None)).is_err());
    assert_eq!(fs::read_dir(&staging).unwrap().count(), 0);
    // Saving over a file the user picked leaves that file as it was.
    let target = written(&folder, "picked.ogg");
    assert!(decrypt_to_file(&take.recorded, &target, &Mutex::new(None)).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"copy");
    assert!(!folder.join(".picked.ogg.partial").exists());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn saving_writes_the_whole_ogg_file_where_the_user_picked() {
    let folder = folder("save");
    let take = sealed_take(&folder, "a", None);
    let target = written(&folder, "picked.ogg");
    let (saved, kind) = decrypt_to_file(&take.recorded, &target, &Mutex::new(None)).unwrap();
    assert_eq!((saved.as_path(), kind), (target.as_path(), CopyKind::Saved));
    let bytes = fs::read(&target).unwrap();
    assert_eq!(&bytes[..4], b"OggS");
    assert_eq!(bytes.len() as u64, take.shown.size_bytes);
    assert!(!folder.join(".picked.ogg.partial").exists());
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn takes_left_by_the_last_run_are_offered_one_after_another() {
    let folder = folder("left-over");
    let keys: Arc<dyn RecordingKeys> = Arc::new(FakeKeys::default());
    let device_key = keys.key().unwrap();
    let conversation = ConversationId::from_snowflake(Snowflake::new(7).unwrap());
    for (name, started_at) in [("recording-1-0", 1_000), ("recording-2-0", 2_000)] {
      let note = serde_json::to_vec(&RecordingNote {
        conversation,
        started_at,
      })
      .unwrap();
      let kept = KeptKey {
        device_key: device_key.duplicate(),
        note,
      };
      // Finished, then the app quit before posting: the take's key is gone from memory.
      drop(sealed_take(&folder, name, Some(kept)));
      std::thread::sleep(Duration::from_millis(20));
    }
    let recording_folder = RecordingFolder::new(folder.clone(), Some(Arc::clone(&keys)));
    let state = Mutex::new(CallState::default());
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    assert!(runtime.block_on(recording_folder.recover(&state)));
    let offered = lock(&state).recordings.take().unwrap();
    assert_eq!(offered.conversation, conversation);
    let left_over = offered.maybe_left_over.unwrap();
    assert_eq!(left_over.started_at, 1_000, "the oldest first");
    assert!(!left_over.is_unfinished);
    recording_folder.discard(&state);
    let next = lock(&state).recordings.take().unwrap();
    assert_eq!(next.maybe_left_over.unwrap().started_at, 2_000);
    recording_folder.discard(&state);
    assert!(lock(&state).recordings.take().is_none());
    assert_eq!(fs::read_dir(&folder).unwrap().count(), 0);
    fs::remove_dir_all(folder).unwrap();
  }

  #[test]
  fn a_start_without_kept_keys_clears_the_folder_and_a_sign_out_forgets_the_key() {
    let folder = folder("cleared");
    fs::write(folder.join("recording-1-0.ogg"), b"sealed").unwrap();
    let keys = Arc::new(FakeKeys::default());
    let recording_folder = RecordingFolder::new(folder.clone(), Some(keys.clone() as Arc<dyn RecordingKeys>));
    assert_eq!(fs::read_dir(&folder).unwrap().count(), 0);
    assert_eq!(keys.reads(), 0, "no kept keys: the local secrets aren't asked");
    keys.key().unwrap();
    let state = Mutex::new(CallState::default());
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    runtime.block_on(recording_folder.sign_out(&state));
    assert!(keys.existing_key().unwrap().is_none());
    fs::remove_dir_all(folder).unwrap();
  }

  /// The recording key of the local secrets, in memory.
  #[derive(Default)]
  struct FakeKeys {
    key: Mutex<Option<SymmetricKey>>,
    reads: AtomicU64,
  }

  impl FakeKeys {
    fn reads(&self) -> u64 {
      self.reads.load(Ordering::Relaxed)
    }
  }

  impl RecordingKeys for FakeKeys {
    fn key(&self) -> Result<SymmetricKey, String> {
      self.reads.fetch_add(1, Ordering::Relaxed);
      let mut key = lock(&self.key);
      Ok(
        key
          .get_or_insert_with(|| SymmetricKey::from_bytes([9; 32]))
          .duplicate(),
      )
    }

    fn existing_key(&self) -> Result<Option<SymmetricKey>, String> {
      self.reads.fetch_add(1, Ordering::Relaxed);
      Ok(lock(&self.key).as_ref().map(SymmetricKey::duplicate))
    }

    fn forget(&self) -> Result<(), String> {
      *lock(&self.key) = None;
      Ok(())
    }
  }

  /// A finished take of one second of silence, sealed in `folder`.
  fn sealed_take(folder: &Path, name: &str, maybe_kept: Option<KeptKey>) -> Take {
    let recorder = CallRecorder::start(
      &folder.join(format!("{name}.ogg")),
      Duration::from_secs(60),
      maybe_kept,
    )
    .unwrap();
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
        maybe_left_over: None,
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
