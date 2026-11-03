//! Downloads to disk that stream and resume (issue #153). The ciphertext goes piece by piece into a
//! partial file named after the attachment; an interrupted download (network loss, an app that was
//! killed) continues there with an HTTP `Range` request instead of starting over. Only once the
//! whole blob matches the hash its message carries is it decrypted, chunk by chunk, into the target
//! file: no unverified plaintext reaches the disk and no step holds the whole file in memory.
//!
//! Every download to disk is recorded ([`DownloadRecord`], newest [`MAX_DOWNLOAD_RECORDS`]): what a
//! restart needs to continue, how often it failed, and the list of recent downloads an app shows.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{AttachmentId, ConversationId};

use super::files::{
  RetryCause, TransferFailure, TransferState, UploadRetry, UploadRetrySettings, failure_of,
};
use crate::error::CoreError;
use crate::records;
use crate::store::{AttachmentRef, LocalStore, Table};

/// Downloads kept in the list; older finished ones are dropped (and their partial files deleted).
pub const MAX_DOWNLOAD_RECORDS: usize = 100;

/// The folder under the upload staging directory that holds partial downloads (ciphertext only).
#[cfg(not(target_arch = "wasm32"))]
const PARTIAL_DIR: &str = "partial-downloads";

/// A download to disk, newest first in [`download_records`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadRecord {
  pub attachment_id: AttachmentId,
  pub conversation_id: ConversationId,
  pub file_name: String,
  /// The plaintext size the message announced.
  pub size_bytes: u64,
  /// When the user asked for it (client clock: only orders the list).
  pub started_at_ms: i64,
  pub target: DownloadTarget,
  pub status: DownloadStatus,
  /// Failed attempts since the user asked; a network failure waits with backoff and continues.
  #[serde(default)]
  pub retry: UploadRetry,
  /// The partial file (ciphertext) while the download is unfinished, so the list can say how far
  /// a paused or waiting one got.
  #[serde(rename = "partial", default, skip_serializing_if = "Option::is_none")]
  pub maybe_partial_path: Option<String>,
}

/// Where a saved file goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownloadTarget {
  /// A new name in the download directory.
  DownloadDirectory,
  /// The exact file the user picked in a save dialog.
  File { path: String },
  /// A new name in the folder the user picked for several files ("Download all", #1014).
  Folder { path: String },
}

/// Where a download stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DownloadStatus {
  /// Transferring, or interrupted by an app that stopped: continued when the app starts again.
  Running,
  /// Failed on the network; continues once [`DownloadRecord::retry`] is due.
  Waiting,
  /// The user paused it; what arrived stays until they continue.
  Paused,
  /// The user cancelled it; what arrived was deleted.
  Cancelled,
  Saved {
    path: String,
  },
  Failed {
    reason: TransferFailure,
  },
}

/// A download whose next attempt is due.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueDownload {
  pub conversation_id: ConversationId,
  pub attachment_id: AttachmentId,
  pub target: DownloadTarget,
}

/// Every recorded download, newest first.
pub fn download_records(store: &impl LocalStore) -> Result<Vec<DownloadRecord>, CoreError> {
  let mut list: Vec<DownloadRecord> = records::list_json(store, Table::Downloads)?;
  list.sort_by_key(|record| std::cmp::Reverse(record.started_at_ms));
  Ok(list)
}

pub fn download_record(
  store: &impl LocalStore,
  attachment: AttachmentId,
) -> Result<Option<DownloadRecord>, CoreError> {
  records::get_json(store, Table::Downloads, &attachment.to_string())
}

/// Drops a finished download from the list (the user cleared it); a running one stays.
pub fn forget_download(store: &impl LocalStore, attachment: AttachmentId) -> Result<bool, CoreError> {
  match download_record(store, attachment)? {
    Some(record) if record.is_finished() => {
      store.delete(Table::Downloads, &attachment.to_string())?;
      Ok(true)
    },
    _ => Ok(false),
  }
}

/// The downloads to continue at `now_ms`: those waiting for a retry that came due and, when
/// `includes_interrupted` (once, when the app starts), those an app that stopped left running.
/// Also returns when the next waiting one comes due.
pub fn due_downloads(
  store: &impl LocalStore,
  now_ms: i64,
  includes_interrupted: bool,
) -> Result<(Vec<DueDownload>, Option<i64>), CoreError> {
  let mut due = Vec::new();
  let mut maybe_next_ms: Option<i64> = None;
  for record in download_records(store)? {
    let is_due = match record.status {
      DownloadStatus::Running => includes_interrupted,
      DownloadStatus::Waiting if record.retry.is_due(now_ms) => true,
      DownloadStatus::Waiting => {
        if let UploadRetry::Waiting { retry_at_ms, .. } = record.retry {
          maybe_next_ms = Some(maybe_next_ms.map_or(retry_at_ms, |next| next.min(retry_at_ms)));
        }
        false
      },
      DownloadStatus::Paused
      | DownloadStatus::Cancelled
      | DownloadStatus::Saved { .. }
      | DownloadStatus::Failed { .. } => false,
    };
    if is_due {
      due.push(DueDownload {
        conversation_id: record.conversation_id,
        attachment_id: record.attachment_id,
        target: record.target,
      });
    }
  }
  Ok((due, maybe_next_ms))
}

/// Records that a download (re)starts. A download asked for again after it finished counts its
/// attempts from zero; one that continues keeps them.
pub(super) fn record_start(
  store: &impl LocalStore,
  conversation: ConversationId,
  reference: &AttachmentRef,
  target: &DownloadTarget,
  now_ms: i64,
) -> Result<(), CoreError> {
  let record = match download_record(store, reference.attachment_id)? {
    Some(record) if !record.is_finished() => DownloadRecord {
      status: DownloadStatus::Running,
      target: target.clone(),
      ..record
    },
    _ => DownloadRecord {
      attachment_id: reference.attachment_id,
      conversation_id: conversation,
      file_name: reference.file_name.clone(),
      size_bytes: reference.size_bytes,
      started_at_ms: now_ms,
      target: target.clone(),
      status: DownloadStatus::Running,
      retry: UploadRetry::Fresh,
      maybe_partial_path: None,
    },
  };
  save(store, &record)?;
  prune(store)
}

/// Records where the download's partial file is.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn record_partial(
  store: &impl LocalStore,
  attachment: AttachmentId,
  partial: &str,
) -> Result<(), CoreError> {
  let Some(mut record) = download_record(store, attachment)? else {
    return Ok(());
  };
  record.maybe_partial_path = Some(partial.to_owned());
  save(store, &record)
}

/// Records that the user paused or cancelled an unfinished download; returns the record before,
/// `None` when there is none or it already finished.
pub(super) fn record_stop(
  store: &impl LocalStore,
  attachment: AttachmentId,
  status: DownloadStatus,
) -> Result<Option<DownloadRecord>, CoreError> {
  let Some(record) = download_record(store, attachment)?.filter(|record| !record.is_finished()) else {
    return Ok(None);
  };
  let is_cancel = status == DownloadStatus::Cancelled;
  save(
    store,
    &DownloadRecord {
      status,
      retry: UploadRetry::Fresh,
      maybe_partial_path: if is_cancel {
        None
      } else {
        record.maybe_partial_path.clone()
      },
      ..record.clone()
    },
  )?;
  Ok(Some(record))
}

/// Records how an attempt ended and returns what the file's card shows: saved or failed (also while
/// a network failure waits for its retry).
pub(super) fn record_finish(
  store: &impl LocalStore,
  attachment: AttachmentId,
  saved: &Result<String, CoreError>,
  now_ms: i64,
  settings: &UploadRetrySettings,
) -> Result<TransferState, CoreError> {
  let Some(mut record) = download_record(store, attachment)? else {
    return Ok(card_state(saved));
  };
  let state = match saved {
    Ok(path) => {
      record.status = DownloadStatus::Saved { path: path.clone() };
      record.maybe_partial_path = None;
      TransferState::Local { path: path.clone() }
    },
    Err(error) => {
      let reason = failure_of(error);
      if is_retried(reason) {
        record
          .retry
          .record_failure_after(now_ms, settings, RetryCause::of(error), retry_after(error));
      }
      // The card says why either way (#742) and offers its retry; a waiting one also comes back
      // by itself.
      record.status = if is_retried(reason) && !record.retry.is_stalled() {
        DownloadStatus::Waiting
      } else {
        DownloadStatus::Failed { reason }
      };
      TransferState::Failed { reason }
    },
  };
  save(store, &record)?;
  Ok(state)
}

impl DownloadRecord {
  pub fn is_finished(&self) -> bool {
    matches!(
      self.status,
      DownloadStatus::Saved { .. } | DownloadStatus::Failed { .. } | DownloadStatus::Cancelled
    )
  }
}

/// What the card shows for a download without a record (one started by an older version).
fn card_state(saved: &Result<String, CoreError>) -> TransferState {
  match saved {
    Ok(path) => TransferState::Local { path: path.clone() },
    Err(error) => TransferState::Failed {
      reason: failure_of(error),
    },
  }
}

/// Failures a later attempt can get through on its own; the rest wait for the user.
fn is_retried(reason: TransferFailure) -> bool {
  matches!(
    reason,
    TransferFailure::Network | TransferFailure::StorageUnavailable
  )
}

fn retry_after(error: &CoreError) -> Option<std::time::Duration> {
  match error {
    CoreError::Api(crate::api::ApiError::Status {
      maybe_retry_after, ..
    }) => *maybe_retry_after,
    _ => None,
  }
}

fn save(store: &impl LocalStore, record: &DownloadRecord) -> Result<(), CoreError> {
  records::put_json(store, Table::Downloads, &record.attachment_id.to_string(), record)
}

/// Keeps the newest [`MAX_DOWNLOAD_RECORDS`]; only finished ones are dropped.
fn prune(store: &impl LocalStore) -> Result<(), CoreError> {
  let list = download_records(store)?;
  for record in list
    .iter()
    .skip(MAX_DOWNLOAD_RECORDS)
    .filter(|record| record.is_finished())
  {
    store.delete(Table::Downloads, &record.attachment_id.to_string())?;
  }
  Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) use native::RangedDownload;
#[cfg(not(target_arch = "wasm32"))]
pub use native::sweep_partial_downloads;

#[cfg(not(target_arch = "wasm32"))]
pub(super) mod native {
  use std::fs::{File, OpenOptions};
  use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
  use std::path::{Path, PathBuf};

  use futures_util::StreamExt;
  use teamcraft_crypto::attachment::{
    AttachmentKey, AttachmentStreamDecryptor, CiphertextHasher, MIN_CHUNK_SIZE, encrypted_len,
  };
  use teamcraft_crypto::secret::SymmetricKey;
  use teamcraft_proto::AttachmentId;

  use super::{PARTIAL_DIR, download_records};
  use crate::api::{ApiError, DetachedRange, RangedBlob};
  use crate::error::CoreError;
  use crate::store::{AttachmentRef, LocalStore};
  use crate::transfers::TransferGuard;

  /// Bytes read from the partial file at a time, for the hash and for decrypting.
  const READ_BYTES: usize = 64 * 1024;

  /// A download to disk on its way: the rest of the blob from where the partial file ends. Dropping
  /// it stops the transfer and keeps what arrived.
  pub struct RangedDownload {
    reference: AttachmentRef,
    partial: PathBuf,
    /// Bytes already in the partial file when this attempt started.
    offset: u64,
    request: DetachedRange,
    progress: TransferGuard,
  }

  /// The whole blob in the partial file, matching the hash its message carries.
  pub(in crate::client) struct VerifiedBlob {
    reference: AttachmentRef,
    partial: PathBuf,
  }

  /// The folder for partial downloads under the upload staging directory (beside the staged
  /// copies, which their sweep leaves alone because it only deletes files).
  fn partial_dir(staging: &Path) -> PathBuf {
    staging.join(PARTIAL_DIR)
  }

  /// Deletes partial files no unfinished download refers to (finished, dropped from the list, or
  /// from another account's store). Returns how many were deleted.
  pub fn sweep_partial_downloads(store: &impl LocalStore, staging: &Path) -> Result<usize, CoreError> {
    let unfinished: std::collections::HashSet<PathBuf> = download_records(store)?
      .iter()
      .filter(|record| !record.is_finished())
      .map(|record| partial_path(staging, record.attachment_id))
      .collect();
    let Ok(entries) = std::fs::read_dir(partial_dir(staging)) else {
      return Ok(0);
    };
    let mut removed = 0;
    for entry in entries.flatten() {
      let path = entry.path();
      if entry.file_type().is_ok_and(|kind| kind.is_file()) && !unfinished.contains(&path) {
        match std::fs::remove_file(&path) {
          Ok(()) => removed += 1,
          Err(error) => tracing::debug!(%error, "a stray partial download could not be deleted"),
        }
      }
    }
    Ok(removed)
  }

  pub(in crate::client) fn partial_path(staging: &Path, attachment: AttachmentId) -> PathBuf {
    partial_dir(staging).join(format!("{attachment}.part"))
  }

  impl RangedDownload {
    /// Starts receiving `reference`'s blob from `url`, after what the partial file already holds.
    pub(in crate::client) fn start(
      reference: AttachmentRef,
      partial: PathBuf,
      progress: TransferGuard,
      request: impl FnOnce(u64) -> DetachedRange,
    ) -> Self {
      let offset = std::fs::metadata(&partial).map_or(0, |meta| meta.len());
      progress.counter().add(offset);
      Self {
        request: request(offset),
        reference,
        partial,
        offset,
        progress,
      }
    }

    pub fn attachment_id(&self) -> AttachmentId {
      self.reference.attachment_id
    }

    pub(in crate::client) fn reference(&self) -> &AttachmentRef {
      &self.reference
    }

    /// Receives the rest of the blob into the partial file and checks it against its hash. After a
    /// network failure the partial file stays for the next attempt; a blob that is not the
    /// announced one is deleted.
    pub(in crate::client) async fn receive(self) -> Result<VerifiedBlob, CoreError> {
      let Self {
        reference,
        partial,
        offset,
        request,
        progress,
      } = self;
      // A storage can't make the client write more than the largest blob the announced size
      // allows (the smallest chunk size has the most tags); the hash check rejects anything else.
      let max_bytes = encrypted_len(reference.size_bytes, MIN_CHUNK_SIZE);
      let RangedBlob { start, mut pieces } = request.await?;
      if start != offset && start != 0 {
        return Err(CoreError::Api(ApiError::Decode(
          "the storage answered a range that was not asked for".into(),
        )));
      }
      let counter = progress.counter();
      if start != offset {
        // The storage ignored the range and sends everything again.
        counter.set(0);
      }
      let mut file = open_partial(&partial, start)?;
      let mut written = start;
      while let Some(piece) = pieces.next().await {
        let piece = piece?;
        written = written.saturating_add(piece.len() as u64);
        if written > max_bytes {
          drop(file);
          discard(&partial);
          return Err(CoreError::Api(ApiError::BlobTooLarge { max_bytes }));
        }
        file.write_all(&piece).map_err(CoreError::LocalFile)?;
        counter.add(piece.len() as u64);
      }
      file.flush().map_err(CoreError::LocalFile)?;
      drop(file);
      if !partial_matches(&partial, reference.ciphertext_sha256.as_slice())? {
        discard(&partial);
        return Err(CoreError::Corrupt(
          "attachment blob doesn't match its hash".into(),
        ));
      }
      Ok(VerifiedBlob { reference, partial })
    }
  }

  impl VerifiedBlob {
    /// Decrypts the blob chunk by chunk into `file` (at `path`, removed again when this fails, so
    /// no cut-off file looks like the real one), then deletes the partial file.
    pub(in crate::client) fn write_into(self, file: File, path: &Path) -> Result<(), CoreError> {
      let decrypted = self.decrypt(file);
      // The blob is the one the sender made either way: kept, it would only fail again.
      discard(&self.partial);
      if decrypted.is_err() {
        // Deliberately ignored: the decrypt or write error is what the user needs.
        let _ = std::fs::remove_file(path);
      }
      decrypted
    }

    fn decrypt(&self, file: File) -> Result<(), CoreError> {
      let key = SymmetricKey::from_slice(self.reference.key.as_slice())
        .ok_or_else(|| CoreError::Corrupt("bad attachment key".into()))?;
      let key = AttachmentKey::from_key(key);
      let mut decryptor = AttachmentStreamDecryptor::new(&key);
      let mut output = BufWriter::new(file);
      let mut maybe_write_error: Option<std::io::Error> = None;
      let mut write = |plaintext: &[u8]| {
        if maybe_write_error.is_none()
          && let Err(error) = output.write_all(plaintext)
        {
          maybe_write_error = Some(error);
        }
      };
      let mut input = File::open(&self.partial).map_err(CoreError::LocalFile)?;
      let mut buffer = vec![0u8; READ_BYTES];
      loop {
        let read = input.read(&mut buffer).map_err(CoreError::LocalFile)?;
        if read == 0 {
          break;
        }
        decryptor.push(&buffer[..read], &mut write)?;
      }
      decryptor.finish(&mut write)?;
      if let Some(error) = maybe_write_error {
        return Err(CoreError::LocalFile(error));
      }
      output.flush().map_err(CoreError::LocalFile)?;
      output
        .into_inner()
        .map_err(|error| CoreError::LocalFile(error.into_error()))?
        .sync_all()
        .map_err(CoreError::LocalFile)
    }
  }

  /// The partial file, cut to `start` bytes and positioned there.
  fn open_partial(partial: &Path, start: u64) -> Result<BufWriter<File>, CoreError> {
    if let Some(parent) = partial.parent() {
      std::fs::create_dir_all(parent).map_err(CoreError::LocalFile)?;
    }
    let mut file = OpenOptions::new()
      .create(true)
      .truncate(false)
      .write(true)
      .open(partial)
      .map_err(CoreError::LocalFile)?;
    file.set_len(start).map_err(CoreError::LocalFile)?;
    file.seek(SeekFrom::Start(start)).map_err(CoreError::LocalFile)?;
    Ok(BufWriter::with_capacity(READ_BYTES, file))
  }

  fn partial_matches(partial: &Path, expected_sha256: &[u8]) -> Result<bool, CoreError> {
    let mut input = File::open(partial).map_err(CoreError::LocalFile)?;
    let mut hasher = CiphertextHasher::new();
    let mut buffer = vec![0u8; READ_BYTES];
    loop {
      let read = input.read(&mut buffer).map_err(CoreError::LocalFile)?;
      if read == 0 {
        return Ok(hasher.finish().as_slice() == expected_sha256);
      }
      hasher.update(&buffer[..read]);
    }
  }

  fn discard(partial: &Path) {
    if let Err(error) = std::fs::remove_file(partial)
      && error.kind() != std::io::ErrorKind::NotFound
    {
      tracing::debug!(%error, "a partial download could not be deleted");
    }
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Base64Bytes, Id, Snowflake};

  use super::*;
  use crate::store::MemoryStore;

  const NOW_MS: i64 = 1_000_000;

  #[test]
  fn a_network_failure_waits_and_comes_due_with_backoff() {
    let store = MemoryStore::new();
    let reference = file(1);
    record_start(
      &store,
      conversation(),
      &reference,
      &DownloadTarget::DownloadDirectory,
      NOW_MS,
    )
    .unwrap();
    let state = record_finish(
      &store,
      reference.attachment_id,
      &network_error(),
      NOW_MS,
      &settings(),
    )
    .unwrap();
    assert_eq!(
      state,
      TransferState::Failed {
        reason: TransferFailure::Network
      }
    );
    let record = download_record(&store, reference.attachment_id).unwrap().unwrap();
    assert_eq!(record.status, DownloadStatus::Waiting);
    let (due, maybe_next) = due_downloads(&store, NOW_MS, false).unwrap();
    assert!(due.is_empty());
    assert_eq!(maybe_next, Some(NOW_MS + 2_000));
    let (due, _) = due_downloads(&store, NOW_MS + 2_000, false).unwrap();
    assert_eq!(due.len(), 1);
  }

  #[test]
  fn after_the_last_attempt_it_fails_for_the_user_to_retry() {
    let store = MemoryStore::new();
    let reference = file(1);
    record_start(
      &store,
      conversation(),
      &reference,
      &DownloadTarget::DownloadDirectory,
      NOW_MS,
    )
    .unwrap();
    let mut state = TransferState::Downloading { total_bytes: 10 };
    for _ in 0..settings().max_attempts {
      record_start(
        &store,
        conversation(),
        &reference,
        &DownloadTarget::DownloadDirectory,
        NOW_MS,
      )
      .unwrap();
      state = record_finish(
        &store,
        reference.attachment_id,
        &network_error(),
        NOW_MS,
        &settings(),
      )
      .unwrap();
    }
    assert_eq!(
      state,
      TransferState::Failed {
        reason: TransferFailure::Network
      }
    );
    // Asked for again, it counts from zero.
    record_start(
      &store,
      conversation(),
      &reference,
      &DownloadTarget::DownloadDirectory,
      NOW_MS,
    )
    .unwrap();
    let record = download_record(&store, reference.attachment_id).unwrap().unwrap();
    assert_eq!(record.retry, UploadRetry::Fresh);
    assert_eq!(record.status, DownloadStatus::Running);
  }

  #[test]
  fn a_corrupt_blob_fails_at_once() {
    let store = MemoryStore::new();
    let reference = file(1);
    record_start(
      &store,
      conversation(),
      &reference,
      &DownloadTarget::DownloadDirectory,
      NOW_MS,
    )
    .unwrap();
    let corrupt = Err(CoreError::Corrupt(
      "attachment blob doesn't match its hash".into(),
    ));
    let state = record_finish(&store, reference.attachment_id, &corrupt, NOW_MS, &settings()).unwrap();
    assert_eq!(
      state,
      TransferState::Failed {
        reason: TransferFailure::Corrupted
      }
    );
  }

  #[test]
  fn running_downloads_are_continued_only_when_the_app_starts() {
    let store = MemoryStore::new();
    record_start(
      &store,
      conversation(),
      &file(1),
      &DownloadTarget::DownloadDirectory,
      NOW_MS,
    )
    .unwrap();
    assert_eq!(due_downloads(&store, NOW_MS, true).unwrap().0.len(), 1);
    assert!(due_downloads(&store, NOW_MS, false).unwrap().0.is_empty());
  }

  #[test]
  fn the_list_is_newest_first_and_keeps_only_the_newest_finished() {
    let store = MemoryStore::new();
    for raw in 1..=(MAX_DOWNLOAD_RECORDS as i64 + 5) {
      let reference = file(raw);
      record_start(
        &store,
        conversation(),
        &reference,
        &DownloadTarget::DownloadDirectory,
        raw,
      )
      .unwrap();
      record_finish(
        &store,
        reference.attachment_id,
        &Ok(format!("/d/{raw}")),
        raw,
        &settings(),
      )
      .unwrap();
    }
    let list = download_records(&store).unwrap();
    assert_eq!(list.len(), MAX_DOWNLOAD_RECORDS);
    assert_eq!(list[0].started_at_ms, MAX_DOWNLOAD_RECORDS as i64 + 5);
    assert!(forget_download(&store, list[0].attachment_id).unwrap());
    assert_eq!(download_records(&store).unwrap().len(), MAX_DOWNLOAD_RECORDS - 1);
  }

  #[test]
  fn records_keep_their_stored_form() {
    let record = DownloadRecord {
      attachment_id: file(7).attachment_id,
      conversation_id: conversation(),
      file_name: "report.pdf".into(),
      size_bytes: 10,
      started_at_ms: 5,
      target: DownloadTarget::Folder { path: "/f".into() },
      status: DownloadStatus::Failed {
        reason: TransferFailure::Missing,
      },
      retry: UploadRetry::Fresh,
      maybe_partial_path: None,
    };
    let json = serde_json::to_value(&record).unwrap();
    assert_eq!(
      json["target"],
      serde_json::json!({"kind": "folder", "path": "/f"})
    );
    assert_eq!(
      json["status"],
      serde_json::json!({"state": "failed", "reason": "missing"})
    );
    assert_eq!(serde_json::from_value::<DownloadRecord>(json).unwrap(), record);
  }

  fn settings() -> UploadRetrySettings {
    UploadRetrySettings::default()
  }

  fn network_error() -> Result<String, CoreError> {
    Err(CoreError::Api(crate::api::ApiError::Network("dropped".into())))
  }

  fn conversation() -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(500).unwrap())
  }

  fn file(raw: i64) -> AttachmentRef {
    AttachmentRef {
      attachment_id: AttachmentId::from_snowflake(Snowflake::new(raw).unwrap()),
      key: Base64Bytes::new(vec![0; 32]),
      ciphertext_sha256: Base64Bytes::new(vec![0; 32]),
      file_name: "report.pdf".into(),
      media_type: "application/pdf".into(),
      size_bytes: 10,
      maybe_thumbnail: None,
      maybe_dimensions: None,
      maybe_duration_ms: None,
    }
  }
}
