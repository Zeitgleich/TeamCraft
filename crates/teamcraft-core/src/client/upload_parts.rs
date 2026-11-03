//! Large files go up in parts (issue #153): each part is read from the file's source, encrypted and
//! uploaded on its own, and the upload's state (key, encryption header, the parts' `ETag`s, the
//! running ciphertext hash) is saved after every part. A failed part is sent again alone, and after
//! a restart the upload continues at the next part. Only one part is in memory at a time, so a
//! 2 GB file needs a few dozen MB, not gigabytes.

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::attachment::{
  AttachmentKey, AttachmentSegmentEncryptor, CiphertextHasher, DEFAULT_CHUNK_SIZE, chunk_count,
  content_sha256, encrypted_len,
};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{
  AttachmentId, Base64Bytes, CompleteMultipartAttachmentRequest, ConversationId, CreateAttachmentRequest,
  ErrorCode, PresignAttachmentPartRequest, UploadedPart,
};
use zeroize::Zeroizing;

use super::Client;
use super::files::{
  MAX_APP_THUMBNAIL_BYTES, PendingFile, TransferFailure, UploadError, UploadSource, is_status, queue_abandon,
};
use crate::api::{ApiError, DetachedPart, ServerApi};
use crate::error::CoreError;
use crate::store::{AttachmentRef, ImageDimensions, LocalStore};
use crate::transfers::TransferGuard;

/// Encrypted chunks per part: 8 MiB of plaintext with the default 64 KiB chunks, well above S3's
/// 5 MiB minimum (each part but the last is a little larger than its plaintext).
pub const PART_CHUNKS: u32 = 128;
const PART_PLAINTEXT_BYTES: u64 = PART_CHUNKS as u64 * DEFAULT_CHUNK_SIZE as u64;

/// Largest picture read whole for its thumbnail when it goes up in parts; bigger ones go without
/// (or with the app's own).
const MAX_THUMBNAIL_SOURCE_BYTES: u64 = 32 * 1024 * 1024;

/// An upload in parts in progress: everything a restart needs to continue at the next part.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartsUpload {
  pub attachment_id: AttachmentId,
  /// The announced ciphertext size.
  pub size_bytes: u64,
  /// The file's key and the encryption header (its nonce prefix): the remaining parts are
  /// encrypted with them exactly like the uploaded ones, so the blob is one stream.
  pub key: Base64Bytes,
  pub header: Base64Bytes,
  /// `ETag`s of the uploaded parts in order; the next part is number `etags.len() + 1`.
  #[serde(default)]
  pub etags: Vec<String>,
  /// Ciphertext bytes of the uploaded parts: where the next part starts in the blob.
  #[serde(default)]
  pub uploaded_bytes: u64,
  /// SHA-256 state over the uploaded parts' ciphertext.
  pub hash_state: Base64Bytes,
  /// When the file was last modified as its upload began, where the platform tells: an edited file
  /// starts over with a fresh key instead of mixing old and new parts.
  #[serde(rename = "modified_ms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_modified_ms: Option<i64>,
  /// The part on its way, by the SHA-256 of its plaintext. Sent again (its answer was lost), it must
  /// be the same plaintext: other bytes under the same key and nonces would give the storage the
  /// XOR of both. A mismatch starts the upload over with a fresh key.
  #[serde(rename = "sending", default, skip_serializing_if = "Option::is_none")]
  pub maybe_sending: Option<SendingPart>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendingPart {
  pub part_number: u16,
  pub plaintext_sha256: Base64Bytes,
}

/// What one step of an upload in parts sends.
pub(super) enum PartStep {
  /// One part, and what the upload's state becomes once it is stored.
  Part {
    part_number: u16,
    plaintext_sha256: [u8; 32],
    ciphertext: Vec<u8>,
    hash_state: Vec<u8>,
    uploaded_bytes: u64,
  },
  /// Every part is up; only joining them remains (an earlier attempt didn't get through).
  Join,
}

/// A part on its way, detached from the client like a whole blob.
pub(super) struct StartedPart {
  pub target: PartTarget,
  pub request: Option<DetachedPart>,
  pub progress: TransferGuard,
}

/// The part a [`super::files::BlobUpload`] sends, to find its file and record it when it's stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PartTarget {
  pub attachment_id: AttachmentId,
  /// `None`: no part is sent, the parts are only joined.
  pub maybe_part: Option<SentPart>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SentPart {
  pub part_number: u16,
  pub hash_state: Vec<u8>,
  pub uploaded_bytes: u64,
}

/// What became of a part.
pub(super) enum PartOutcome {
  /// Stored; more parts follow.
  Stored,
  /// The last one is stored and the parts are joined: the file is up.
  Uploaded(AttachmentRef),
}

/// Whether a file of `plaintext_len` bytes goes up in parts (more than one part's worth).
pub fn is_multipart(plaintext_len: u64) -> bool {
  chunk_count(plaintext_len, DEFAULT_CHUNK_SIZE) > u64::from(PART_CHUNKS)
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Starts the next step of a large file's upload: announces it first if it has no upload in parts
  /// yet, then reads, encrypts and sends its next part (or only joins the parts, when all are up).
  /// `Ok(None)`: the server can't take uploads in parts; the file goes up whole.
  pub(super) async fn start_part(
    &mut self,
    conversation: ConversationId,
    file: &mut PendingFile,
  ) -> Result<Option<StartedPart>, UploadError> {
    let modified_ms = source_modified_ms(&file.source).await;
    if file
      .maybe_parts
      .as_ref()
      .is_some_and(|parts| parts.maybe_modified_ms != modified_ms)
    {
      tracing::info!("a file changed during its upload; it starts over");
      self.start_over(file)?;
    }
    // At most twice: a part that doesn't match what was sent before starts the upload over once.
    let (parts, step) = loop {
      if file.maybe_parts.is_none() {
        let Some(mut parts) = self.announce_parts(conversation, file).await? else {
          return Ok(None);
        };
        parts.maybe_modified_ms = modified_ms;
        file.maybe_parts = Some(parts);
      }
      let Some(parts) = file.maybe_parts.clone() else {
        return Err(UploadError::Failed(TransferFailure::LocalFile));
      };
      let step = next_part(&parts, &file.source, file.size_bytes).await?;
      if is_resent_differently(&parts, &step) {
        tracing::info!("a part to send again has other bytes; the upload starts over");
        self.start_over(file)?;
        continue;
      }
      break (parts, step);
    };
    let progress = self.upload_progress(&parts);
    let PartStep::Part {
      part_number,
      plaintext_sha256,
      ciphertext,
      hash_state,
      uploaded_bytes,
    } = step
    else {
      return Ok(Some(StartedPart {
        target: PartTarget {
          attachment_id: parts.attachment_id,
          maybe_part: None,
        },
        request: None,
        progress,
      }));
    };
    if let Some(parts) = file.maybe_parts.as_mut() {
      // Saved (by the caller) before the part leaves, so a restart knows what it sent.
      parts.maybe_sending = Some(SendingPart {
        part_number,
        plaintext_sha256: Base64Bytes::new(plaintext_sha256.to_vec()),
      });
    }
    let url = self
      .api
      .presign_attachment_part(
        parts.attachment_id,
        &PresignAttachmentPartRequest {
          part_number,
          size_bytes: ciphertext.len() as u64,
        },
      )
      .await
      .map_err(|error| parts_failure(file, error))?;
    let counter = progress.counter();
    counter.set(parts.uploaded_bytes);
    let request = self.api.put_part_detached(&url.upload_url, ciphertext, counter);
    Ok(Some(StartedPart {
      target: PartTarget {
        attachment_id: parts.attachment_id,
        maybe_part: Some(SentPart {
          part_number,
          hash_state,
          uploaded_bytes,
        }),
      },
      request: Some(request),
      progress,
    }))
  }

  /// Records a part the host sent (`sent` carries its `ETag`) and joins the parts after the last
  /// one. The upload's state is saved by the caller after every part.
  pub(super) async fn finish_part(
    &mut self,
    target: PartTarget,
    sent: Result<Option<String>, ApiError>,
    file: &mut PendingFile,
  ) -> Result<PartOutcome, UploadError> {
    if !self.record_part(target, sent, file).await? {
      return Ok(PartOutcome::Stored);
    }
    self.join_parts(file).await.map(PartOutcome::Uploaded)
  }

  /// Records a part the host sent (`sent` carries its `ETag`). `Ok(true)`: every part is up and
  /// only joining them remains ([`Self::join_parts`]).
  pub(super) async fn record_part(
    &mut self,
    target: PartTarget,
    sent: Result<Option<String>, ApiError>,
    file: &mut PendingFile,
  ) -> Result<bool, UploadError> {
    let Some(part) = target.maybe_part else {
      return Ok(true);
    };
    let etag = match sent {
      Ok(Some(etag)) => etag,
      Ok(None) => return Err(UploadError::Failed(TransferFailure::LocalFile)),
      Err(error) => {
        return Err(
          self
            .part_send_failure(target.attachment_id, &part, file, error)
            .await,
        );
      },
    };
    let Some(parts) = file.maybe_parts.as_mut() else {
      return Err(UploadError::Failed(TransferFailure::LocalFile));
    };
    parts.etags.push(etag);
    parts.maybe_sending = None;
    parts.hash_state = Base64Bytes::new(part.hash_state);
    parts.uploaded_bytes = part.uploaded_bytes;
    Ok(parts.uploaded_bytes >= parts.size_bytes)
  }

  /// Gives up the file's upload in parts (at the server, once reachable) so the next step announces
  /// a new one with a fresh key.
  fn start_over(&self, file: &mut PendingFile) -> Result<(), UploadError> {
    if let Some(parts) = file.maybe_parts.take() {
      queue_abandon(&self.store, parts.attachment_id).map_err(UploadError::Retry)?;
    }
    Ok(())
  }

  /// Announces the file's upload in parts with a fresh key and keeps its thumbnail and size for the
  /// pending message. `Ok(None)`: the server has no uploads in parts (an older version).
  async fn announce_parts(
    &self,
    conversation: ConversationId,
    file: &mut PendingFile,
  ) -> Result<Option<PartsUpload>, UploadError> {
    self.keep_picture_details(file).await;
    let size_bytes = encrypted_len(file.size_bytes, DEFAULT_CHUNK_SIZE);
    let request = CreateAttachmentRequest {
      conversation_id: conversation,
      size_bytes,
    };
    let created = match self.api.create_multipart_attachment(&request).await {
      Ok(created) => created,
      Err(error) if error.is_unreachable() => return Err(UploadError::Offline(CoreError::Api(error))),
      Err(error) if is_missing_endpoint(&error) => return Ok(None),
      Err(error) if error.error_code() == Some(&ErrorCode::PayloadTooLarge) || is_status(&error, 413) => {
        return Err(UploadError::Failed(TransferFailure::TooLarge));
      },
      Err(error) => return Err(UploadError::Retry(CoreError::Api(error))),
    };
    let key = AttachmentKey::generate(&mut OsRng);
    let encryptor = AttachmentSegmentEncryptor::new(&key, DEFAULT_CHUNK_SIZE, &mut OsRng)
      .map_err(|_| UploadError::Failed(TransferFailure::LocalFile))?;
    Ok(Some(PartsUpload {
      attachment_id: created.id,
      size_bytes,
      key: Base64Bytes::new(key.as_key().expose_secret().to_vec()),
      header: Base64Bytes::new(encryptor.header().to_vec()),
      etags: Vec::new(),
      uploaded_bytes: 0,
      hash_state: Base64Bytes::new(CiphertextHasher::new().state()),
      maybe_modified_ms: None,
      maybe_sending: None,
    }))
  }

  /// A picture small enough to read whole gets the core's thumbnail and its size, like a file that
  /// goes up whole; a bigger one keeps the app's thumbnail, if any.
  async fn keep_picture_details(&self, file: &mut PendingFile) {
    // A host that makes its own thumbnails handed them in with the file.
    if !self.settings.makes_thumbnails
      || !file.media_type.starts_with("image/")
      || file.size_bytes > MAX_THUMBNAIL_SOURCE_BYTES
    {
      return;
    }
    let Ok(content) = read_segment(&file.source, 0, file.size_bytes, file.size_bytes).await else {
      return;
    };
    if let Some(thumbnail) = crate::thumbnail::make_thumbnail(&content, &file.media_type).await {
      file.maybe_thumbnail = Some(Base64Bytes::new(thumbnail));
    }
    file.maybe_dimensions = file.maybe_dimensions.or_else(|| {
      crate::thumbnail::image_dimensions(&content, &file.media_type)
        .map(|(width, height)| ImageDimensions { width, height })
    });
  }

  /// The upload's progress entry: the one kept from its previous part, so the pending message
  /// doesn't flicker between parts, or a new one.
  fn upload_progress(&mut self, parts: &PartsUpload) -> TransferGuard {
    match self.held_upload_progress.take() {
      Some(progress) if progress.attachment_id() == parts.attachment_id => progress,
      _ => self.transfers.start(parts.attachment_id, parts.size_bytes),
    }
  }

  /// A part that didn't arrive. Was it the storage or the whole network? Asking the server for the
  /// part's URL again tells (it also keeps the upload from being swept): unreachable means offline.
  /// A storage that no longer knows the upload (404) makes the next attempt start over.
  async fn part_send_failure(
    &self,
    attachment: AttachmentId,
    part: &SentPart,
    file: &mut PendingFile,
    error: ApiError,
  ) -> UploadError {
    if error.is_unreachable() {
      let probe = PresignAttachmentPartRequest {
        part_number: part.part_number,
        size_bytes: 1,
      };
      return match self.api.presign_attachment_part(attachment, &probe).await {
        Err(probe) if probe.is_unreachable() => UploadError::Offline(CoreError::Api(error)),
        Err(probe) => parts_failure(file, probe),
        Ok(_) => UploadError::Retry(CoreError::Api(error)),
      };
    }
    if is_status(&error, 404) {
      tracing::debug!(%error, "the storage lost the upload in parts; the next attempt starts over");
      file.maybe_parts = None;
    }
    UploadError::Retry(CoreError::Api(error))
  }

  /// Joins the uploaded parts into the blob and makes the file's reference for its message.
  pub(super) async fn join_parts(&self, file: &mut PendingFile) -> Result<AttachmentRef, UploadError> {
    let Some(parts) = file.maybe_parts.clone() else {
      return Err(UploadError::Failed(TransferFailure::LocalFile));
    };
    let request = CompleteMultipartAttachmentRequest {
      parts: parts
        .etags
        .iter()
        .enumerate()
        .map(|(index, etag)| UploadedPart {
          part_number: part_number(index),
          etag: etag.clone(),
        })
        .collect(),
    };
    self
      .api
      .complete_multipart_attachment(parts.attachment_id, &request)
      .await
      .map_err(|error| parts_failure(file, error))?;
    let sha256 = CiphertextHasher::from_state(parts.hash_state.as_slice())
      .map_err(|_| UploadError::Failed(TransferFailure::LocalFile))?
      .finish();
    Ok(AttachmentRef {
      attachment_id: parts.attachment_id,
      key: parts.key.clone(),
      ciphertext_sha256: Base64Bytes::new(sha256.to_vec()),
      file_name: file.file_name.clone(),
      media_type: file.media_type.clone(),
      size_bytes: file.size_bytes,
      maybe_thumbnail: file
        .maybe_thumbnail
        .clone()
        .filter(|thumbnail| thumbnail.as_slice().len() <= MAX_APP_THUMBNAIL_BYTES),
      maybe_dimensions: file.maybe_dimensions,
      maybe_duration_ms: file.maybe_duration_ms,
    })
  }
}

/// Reads and encrypts the upload's next part, and works out the state after it.
async fn next_part(
  parts: &PartsUpload,
  source: &UploadSource,
  plaintext_len: u64,
) -> Result<PartStep, UploadError> {
  if parts.uploaded_bytes >= parts.size_bytes {
    return Ok(PartStep::Join);
  }
  let index = parts.etags.len();
  let start = index as u64 * PART_PLAINTEXT_BYTES;
  let length = PART_PLAINTEXT_BYTES.min(plaintext_len.saturating_sub(start));
  let is_last = start + length >= plaintext_len;
  let local_failure = || UploadError::Failed(TransferFailure::LocalFile);
  let plaintext = read_segment(source, start, length, plaintext_len)
    .await
    .map_err(|()| local_failure())?;
  let plaintext_sha256 = content_sha256(&plaintext);
  let key = SymmetricKey::from_slice(parts.key.as_slice()).ok_or_else(local_failure)?;
  let encryptor = AttachmentSegmentEncryptor::resume(&AttachmentKey::from_key(key), parts.header.as_slice())
    .map_err(|_| local_failure())?;
  let first_chunk = u32::try_from(index as u64 * u64::from(PART_CHUNKS)).map_err(|_| local_failure())?;
  let mut ciphertext = if index == 0 {
    encryptor.header().to_vec()
  } else {
    Vec::new()
  };
  encryptor
    .encrypt_segment_into(first_chunk, &plaintext, is_last, &mut ciphertext)
    .map_err(|_| local_failure())?;
  drop(plaintext);
  let mut hasher = CiphertextHasher::from_state(parts.hash_state.as_slice()).map_err(|_| local_failure())?;
  hasher.update(&ciphertext);
  Ok(PartStep::Part {
    part_number: part_number(index),
    plaintext_sha256,
    uploaded_bytes: parts.uploaded_bytes + ciphertext.len() as u64,
    hash_state: hasher.state(),
    ciphertext,
  })
}

/// Whether `step` sends a part again (the one on its way before) with other plaintext.
fn is_resent_differently(parts: &PartsUpload, step: &PartStep) -> bool {
  let (
    Some(sending),
    PartStep::Part {
      part_number,
      plaintext_sha256,
      ..
    },
  ) = (&parts.maybe_sending, step)
  else {
    return false;
  };
  sending.part_number == *part_number && sending.plaintext_sha256.as_slice() != plaintext_sha256.as_slice()
}

/// When the file was last modified, in Unix milliseconds, where the platform tells (a file on
/// disk, a file picked in the browser); bytes held in the store never change.
async fn source_modified_ms(source: &UploadSource) -> Option<i64> {
  match source {
    UploadSource::Bytes { .. } => None,
    #[cfg(not(target_arch = "wasm32"))]
    UploadSource::Path { path } => std::fs::metadata(path)
      .and_then(|metadata| metadata.modified())
      .ok()
      .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
      .and_then(|since| i64::try_from(since.as_millis()).ok()),
    #[cfg(target_arch = "wasm32")]
    UploadSource::Path { .. } => None,
    UploadSource::Picked { handle } => crate::picked_files::modified_ms(handle).await,
  }
}

/// The `length` bytes of the file at `start`. A file whose size changed since it was queued is
/// refused: its parts would no longer fit together.
async fn read_segment(
  source: &UploadSource,
  start: u64,
  length: u64,
  expected_len: u64,
) -> Result<Zeroizing<Vec<u8>>, ()> {
  let end = start.checked_add(length).ok_or(())?;
  match source {
    UploadSource::Bytes { bytes } => {
      let bytes = bytes.as_slice();
      if bytes.len() as u64 != expected_len {
        return Err(());
      }
      let range = usize::try_from(start).map_err(|_| ())?..usize::try_from(end).map_err(|_| ())?;
      bytes
        .get(range)
        .map(|slice| Zeroizing::new(slice.to_vec()))
        .ok_or(())
    },
    #[cfg(not(target_arch = "wasm32"))]
    UploadSource::Path { path } => read_file_segment(path, start, length, expected_len),
    #[cfg(target_arch = "wasm32")]
    UploadSource::Path { .. } => Err(()),
    UploadSource::Picked { handle } => crate::picked_files::read(handle, start, end, expected_len)
      .await
      .ok_or(()),
  }
}

#[cfg(not(target_arch = "wasm32"))]
fn read_file_segment(
  path: &str,
  start: u64,
  length: u64,
  expected_len: u64,
) -> Result<Zeroizing<Vec<u8>>, ()> {
  use std::io::{Read, Seek, SeekFrom};

  let read = || -> std::io::Result<Zeroizing<Vec<u8>>> {
    let mut file = std::fs::File::open(path)?;
    if file.metadata()?.len() != expected_len {
      return Err(std::io::Error::other("the file changed since it was queued"));
    }
    file.seek(SeekFrom::Start(start))?;
    let mut buffer = Zeroizing::new(vec![0; usize::try_from(length).map_err(std::io::Error::other)?]);
    file.read_exact(&mut buffer)?;
    Ok(buffer)
  };
  read().map_err(|error| tracing::warn!(%error, "could not read a part of a file to upload"))
}

/// Part numbers count from 1; S3 has at most 10 000, far more than the largest allowed file needs.
fn part_number(index: usize) -> u16 {
  u16::try_from(index + 1).unwrap_or(u16::MAX)
}

/// The server answered a request of the upload in parts with an error. An upload it no longer
/// knows (404: swept, given up) or can't join (409) starts over on the next attempt, with a new
/// announcement; anything else is retried as it is.
fn parts_failure(file: &mut PendingFile, error: ApiError) -> UploadError {
  if error.is_unreachable() {
    return UploadError::Offline(CoreError::Api(error));
  }
  if (is_status(&error, 404) || is_status(&error, 409)) && !error.is_sign_in_required() {
    tracing::debug!(%error, "the upload in parts is gone; the next attempt starts over");
    file.maybe_parts = None;
  }
  UploadError::Retry(CoreError::Api(error))
}

/// A server without uploads in parts answers their routes with 404 or 405 and no error code.
fn is_missing_endpoint(error: &ApiError) -> bool {
  (is_status(error, 404) || is_status(error, 405)) && error.error_code().is_none()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_files_larger_than_one_part_go_up_in_parts() {
    assert!(!is_multipart(0));
    assert!(!is_multipart(PART_PLAINTEXT_BYTES));
    assert!(is_multipart(PART_PLAINTEXT_BYTES + 1));
  }

  #[test]
  fn parts_but_the_last_are_above_the_storage_minimum() {
    let part = encrypted_len(2 * PART_PLAINTEXT_BYTES, DEFAULT_CHUNK_SIZE) / 2;
    assert!(part >= 5 * 1024 * 1024);
    assert!(part < 9 * 1024 * 1024);
  }
}
