//! Attachments (e2ee.md §4.5): clients encrypt, upload and download blobs through presigned URLs.
//! The server records who may fetch a blob and checks the uploaded size; keys and names stay in
//! the encrypted message.

use std::sync::Arc;

use teamcraft_db::{NewAttachment, RemovedAttachment};
use teamcraft_proto::{
  AttachmentDownloadResponse, AttachmentId, AttachmentPartUrlResponse, AttachmentResponse,
  CompleteMultipartAttachmentRequest, CreateAttachmentRequest, CreateAttachmentResponse,
  CreateMultipartAttachmentResponse, HistoryPolicy, Id, Permission, PresignAttachmentPartRequest, UnixMillis,
};
use time::OffsetDateTime;
use tracing::warn;

use super::access::{is_guest, load_conversation, require};
use super::convert::unix_millis;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;
use crate::storage::{BlobStore, StorageError};

/// S3's limits for uploads in parts: every part but the last has at least 5 MiB, and there are at
/// most 10 000 parts.
const MIN_PART_SIZE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_PARTS: u16 = 10_000;
/// Largest part this server presigns (clients send about 8 MiB). With the part numbers limited to
/// what the announced size needs at the minimum part size, the storage an upload can fill, parts
/// sent again included, stays within about three times its announced (quota-checked) size.
const MAX_PART_SIZE_BYTES: u64 = 16 * 1024 * 1024;
/// Longest `ETag` taken from a client; S3's are quoted hex digests, far shorter.
const MAX_ETAG_LEN: usize = 256;

/// Removes attachments whose upload never completed and whose upload URL expired before `before`
/// (see [`super::backgrounds::sweep_cutoff`]), with any object a late upload left behind. Without
/// this, every upload that a client gave up on (a flaky network retries with a new attachment)
/// counted against `max_pending_attachments_per_user` forever and eventually blocked all uploads.
/// Uploads in parts are aborted once they got no new part for `multipart_expiry_secs` more.
pub async fn sweep_unfinished(state: &AppState, before: OffsetDateTime) -> Result<usize, ApiError> {
  let multipart_expiry = state
    .maybe_blob_store
    .as_ref()
    .map_or(std::time::Duration::ZERO, |store| store.multipart_expiry());
  sweep_unfinished_before(state, before, before - multipart_expiry).await
}

/// [`sweep_unfinished`] with the cut-off for uploads in parts given explicitly.
pub async fn sweep_unfinished_before(
  state: &AppState,
  before: OffsetDateTime,
  multipart_before: OffsetDateTime,
) -> Result<usize, ApiError> {
  let Some(store) = state.maybe_blob_store.as_ref() else {
    return Ok(0);
  };
  if store.check_available().is_err() {
    // As for backgrounds: rows go first, so nothing is swept while the storage is known to be down.
    warn!("the storage is down; sweeping unfinished attachments later");
    return Ok(0);
  }
  let removed = state
    .database
    .sweep_unfinished_attachments(before, multipart_before)
    .await?;
  for attachment in &removed {
    free_storage(store, attachment).await;
  }
  Ok(removed.len())
}

pub async fn create_attachment(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateAttachmentRequest,
) -> Result<CreateAttachmentResponse, ApiError> {
  let store = blob_store(state)?;
  // While the storage is down, say "later" here instead of handing out a URL that fails.
  store.check_available()?;
  check_announcement(state, caller, &request).await?;
  let id: AttachmentId = state.next_id()?;
  let key = object_key(id);
  state
    .database
    .create_attachment(&NewAttachment {
      id,
      uploader: caller.user.id,
      conversation: request.conversation_id,
      size_bytes: request.size_bytes,
      s3_key: &key,
      maybe_multipart_upload_id: None,
    })
    .await?;
  Ok(CreateAttachmentResponse {
    id,
    upload_url: store.presign_upload(&key, request.size_bytes).to_string(),
    expires_at: expires_at(store),
  })
}

/// Announces a large upload that goes up in parts ([`presign_part`], then
/// [`complete_multipart_attachment`]); the same checks and limits as [`create_attachment`].
pub async fn create_multipart_attachment(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateAttachmentRequest,
) -> Result<CreateMultipartAttachmentResponse, ApiError> {
  let store = blob_store(state)?;
  check_announcement(state, caller, &request).await?;
  let id: AttachmentId = state.next_id()?;
  let key = object_key(id);
  let upload_id = store.create_multipart(&key).await.map_err(storage_unavailable)?;
  let created = state
    .database
    .create_attachment(&NewAttachment {
      id,
      uploader: caller.user.id,
      conversation: request.conversation_id,
      size_bytes: request.size_bytes,
      s3_key: &key,
      maybe_multipart_upload_id: Some(&upload_id),
    })
    .await;
  if let Err(error) = created {
    if let Err(abort) = store.abort_multipart(&key, &upload_id).await {
      warn!(error = %abort, "aborting an unrecorded upload in parts failed");
    }
    return Err(error.into());
  }
  Ok(CreateMultipartAttachmentResponse {
    id,
    min_part_size_bytes: MIN_PART_SIZE_BYTES,
    max_part_size_bytes: MAX_PART_SIZE_BYTES,
    max_parts: max_parts(request.size_bytes),
    expires_at: unix_millis(OffsetDateTime::now_utc() + store.multipart_expiry()),
  })
}

/// A presigned URL for one part of the caller's unfinished upload in parts. Asking for a part keeps
/// the upload from being swept.
pub async fn presign_part(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: AttachmentId,
  request: PresignAttachmentPartRequest,
) -> Result<AttachmentPartUrlResponse, ApiError> {
  let store = blob_store(state)?;
  // While the storage is down, say "later" here instead of handing out a URL that fails.
  store.check_available()?;
  let attachment = own_attachment(state, caller, id).await?;
  if attachment.is_complete {
    return Err(ApiError::Conflict("the upload has been completed".into()));
  }
  let upload_id = attachment
    .maybe_multipart_upload_id
    .ok_or_else(|| ApiError::Conflict("the attachment is not uploaded in parts".into()))?;
  check_part(attachment.size_bytes, &request)?;
  state.database.touch_attachment(id).await?;
  Ok(AttachmentPartUrlResponse {
    upload_url: store
      .presign_upload_part(
        &attachment.s3_key,
        &upload_id,
        request.part_number,
        request.size_bytes,
      )
      .to_string(),
    expires_at: expires_at(store),
  })
}

/// Joins the parts of the caller's upload and confirms it like [`complete_attachment`]. Completing
/// again (the first answer was lost) is harmless.
pub async fn complete_multipart_attachment(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: AttachmentId,
  request: CompleteMultipartAttachmentRequest,
) -> Result<AttachmentResponse, ApiError> {
  let store = blob_store(state)?;
  let attachment = own_attachment(state, caller, id).await?;
  if !attachment.is_complete {
    let etags = part_etags(&request)?;
    match &attachment.maybe_multipart_upload_id {
      Some(upload_id) => match store
        .complete_multipart(&attachment.s3_key, upload_id, &etags)
        .await
      {
        Ok(()) => {},
        // Joined by an earlier request whose answer was lost: the size check below decides.
        Err(StorageError::MultipartRefused(code)) if code == "NoSuchUpload" => {},
        Err(StorageError::MultipartRefused(code)) => {
          return Err(ApiError::Conflict(format!(
            "the parts could not be joined ({code})"
          )));
        },
        Err(error) => return Err(storage_unavailable(error)),
      },
      None => {
        return Err(ApiError::Conflict(
          "the attachment is not uploaded in parts".into(),
        ));
      },
    }
    confirm_size(state, caller, store, &attachment).await?;
  }
  attachment_response(&attachment)
}

/// The uploader gives up on an unfinished upload (cancelled): its parts or object are freed and it
/// no longer counts against the unfinished uploads. A completed attachment stays (its message may
/// carry it); one that is gone counts as given up.
pub async fn abandon_attachment(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: AttachmentId,
) -> Result<(), ApiError> {
  let store = blob_store(state)?;
  match state
    .database
    .delete_unfinished_attachment(id, caller.user.id)
    .await?
  {
    Some(removed) => {
      free_storage(store, &removed).await;
      Ok(())
    },
    None => match state.database.get_attachment(id).await? {
      Some(attachment) if attachment.maybe_uploader == Some(caller.user.id) => {
        Err(ApiError::Conflict("the upload has been completed".into()))
      },
      _ => Ok(()),
    },
  }
}

/// Confirms the upload: the stored object must have exactly the announced size.
pub async fn complete_attachment(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: AttachmentId,
) -> Result<AttachmentResponse, ApiError> {
  let store = blob_store(state)?;
  let attachment = own_attachment(state, caller, id).await?;
  if !attachment.is_complete {
    if attachment.maybe_multipart_upload_id.is_some() {
      return Err(ApiError::Conflict("the attachment is uploaded in parts".into()));
    }
    confirm_size(state, caller, store, &attachment).await?;
  }
  attachment_response(&attachment)
}

/// A download URL for members of the attachment's conversation (guests and `from_join` channels:
/// only for attachments created after joining).
pub async fn download_attachment(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: AttachmentId,
) -> Result<AttachmentDownloadResponse, ApiError> {
  let store = blob_store(state)?;
  // While the storage is down, say "later" here instead of handing out a URL that fails.
  store.check_available()?;
  let (attachment, _) = readable_attachment(state, caller, id).await?;
  Ok(AttachmentDownloadResponse {
    download_url: store.presign_download(&attachment.s3_key).to_string(),
    size_bytes: attachment.size_bytes,
    expires_at: expires_at(store),
  })
}

/// A completed attachment the caller may read, with the conversation it belongs to.
pub async fn readable_attachment(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: AttachmentId,
) -> Result<(teamcraft_db::Attachment, super::access::ConversationAccess), ApiError> {
  let attachment = state
    .database
    .get_attachment(id)
    .await?
    .ok_or_else(ApiError::attachment_not_found)?;
  let conversation = attachment
    .maybe_conversation
    .ok_or_else(ApiError::attachment_not_found)?;
  let access = load_conversation(state, caller, conversation)
    .await
    .map_err(|_| ApiError::attachment_not_found())?;
  let Some(membership) = &access.view.maybe_membership else {
    return Err(ApiError::attachment_not_found());
  };
  let sees_all_history = access.view.conversation.history_policy == HistoryPolicy::Full && !is_guest(caller);
  if !sees_all_history && id.snowflake() < membership.joined_floor.snowflake() {
    return Err(ApiError::attachment_not_found());
  }
  if !attachment.is_complete {
    return Err(ApiError::Conflict("the upload has not been completed".into()));
  }
  Ok((attachment, access))
}

/// The permission, size and quota checks every announced upload goes through.
async fn check_announcement(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: &CreateAttachmentRequest,
) -> Result<(), ApiError> {
  let access = load_conversation(state, caller, request.conversation_id).await?;
  require(state, caller, Permission::CreatePost, &access.target())?;
  if request.size_bytes == 0 {
    return Err(ApiError::InvalidRequest("size_bytes must be positive".into()));
  }
  let max_attachment_bytes = state.settings.get().max_attachment_bytes;
  if request.size_bytes > max_attachment_bytes {
    return Err(ApiError::PayloadTooLarge(format!(
      "attachments are limited to {} bytes",
      max_attachment_bytes
    )));
  }
  let limits = &state.settings.get().limits;
  let (pending, stored_bytes) = state.database.attachment_usage_of(caller.user.id).await?;
  if pending >= limits.max_pending_attachments_per_user as u64 {
    return Err(ApiError::Conflict(
      "too many unfinished uploads; finish or wait for them first".into(),
    ));
  }
  if stored_bytes.saturating_add(request.size_bytes) > limits.max_attachment_bytes_per_user {
    return Err(ApiError::PayloadTooLarge(
      "your attachment storage is full".into(),
    ));
  }
  Ok(())
}

/// An attachment the caller uploads (anyone else's is "not found").
async fn own_attachment(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: AttachmentId,
) -> Result<teamcraft_db::Attachment, ApiError> {
  let attachment = state
    .database
    .get_attachment(id)
    .await?
    .ok_or_else(ApiError::attachment_not_found)?;
  if attachment.maybe_uploader != Some(caller.user.id) {
    return Err(ApiError::attachment_not_found());
  }
  Ok(attachment)
}

/// Marks the attachment complete once the stored object has exactly the announced size. An upload
/// in parts that came out at another size is removed, so the client starts over.
async fn confirm_size(
  state: &AppState,
  caller: &AuthenticatedUser,
  store: &BlobStore,
  attachment: &teamcraft_db::Attachment,
) -> Result<(), ApiError> {
  let stored = store
    .object_size(&attachment.s3_key)
    .await
    .map_err(storage_unavailable)?;
  match stored {
    None => Err(ApiError::Conflict("the blob has not been uploaded".into())),
    Some(size) if size != attachment.size_bytes => {
      if attachment.maybe_multipart_upload_id.is_some()
        && let Some(removed) = state
          .database
          .delete_unfinished_attachment(attachment.id, caller.user.id)
          .await?
      {
        free_storage(
          store,
          &RemovedAttachment {
            maybe_multipart_upload_id: None,
            ..removed
          },
        )
        .await;
      }
      Err(ApiError::Conflict(format!(
        "the uploaded blob has {size} bytes, {} were announced",
        attachment.size_bytes
      )))
    },
    Some(_) => Ok(state.database.complete_attachment(attachment.id).await?),
  }
}

fn attachment_response(attachment: &teamcraft_db::Attachment) -> Result<AttachmentResponse, ApiError> {
  Ok(AttachmentResponse {
    id: attachment.id,
    conversation_id: attachment
      .maybe_conversation
      .ok_or_else(ApiError::attachment_not_found)?,
    size_bytes: attachment.size_bytes,
    is_complete: true,
  })
}

/// Most parts an upload of `size_bytes` needs: all but the last at the minimum part size.
fn max_parts(size_bytes: u64) -> u16 {
  let parts = size_bytes.div_ceil(MIN_PART_SIZE_BYTES).max(1);
  u16::try_from(parts).unwrap_or(MAX_PARTS).min(MAX_PARTS)
}

/// A part of an upload of `size_bytes` may exist and has a size the upload can hold: the parts
/// before it take at least the minimum part size each, so it gets at most what is left after them.
fn check_part(size_bytes: u64, request: &PresignAttachmentPartRequest) -> Result<(), ApiError> {
  let max_parts = max_parts(size_bytes);
  if !(1..=max_parts).contains(&request.part_number) {
    return Err(ApiError::InvalidRequest(format!(
      "part_number must be between 1 and {max_parts}"
    )));
  }
  let before = u64::from(request.part_number - 1) * MIN_PART_SIZE_BYTES;
  let max_part_bytes = size_bytes.saturating_sub(before).min(MAX_PART_SIZE_BYTES);
  if request.size_bytes == 0 || request.size_bytes > max_part_bytes {
    return Err(ApiError::InvalidRequest(format!(
      "size_bytes of part {} must be between 1 and {max_part_bytes}",
      request.part_number
    )));
  }
  Ok(())
}

/// The parts' `ETag`s in part order; the parts must be numbered 1 to n without gaps.
fn part_etags(request: &CompleteMultipartAttachmentRequest) -> Result<Vec<String>, ApiError> {
  if request.parts.is_empty() || request.parts.len() > usize::from(MAX_PARTS) {
    return Err(ApiError::InvalidRequest(format!(
      "an upload has 1 to {MAX_PARTS} parts"
    )));
  }
  let mut parts: Vec<_> = request.parts.iter().collect();
  parts.sort_by_key(|part| part.part_number);
  let is_numbered = parts
    .iter()
    .enumerate()
    .all(|(index, part)| usize::from(part.part_number) == index + 1);
  if !is_numbered {
    return Err(ApiError::InvalidRequest(
      "parts must be numbered 1 to n without gaps".into(),
    ));
  }
  if parts
    .iter()
    .any(|part| part.etag.is_empty() || part.etag.len() > MAX_ETAG_LEN)
  {
    return Err(ApiError::InvalidRequest("a part has no valid etag".into()));
  }
  Ok(parts.into_iter().map(|part| part.etag.clone()).collect())
}

/// Frees what an unfinished attachment left in the storage; failures are logged (a bucket
/// lifecycle rule or the next sweep catches leftovers).
async fn free_storage(store: &BlobStore, removed: &RemovedAttachment) {
  let freed = match &removed.maybe_multipart_upload_id {
    Some(upload_id) => store.abort_multipart(&removed.s3_key, upload_id).await,
    None => store.delete_object(&removed.s3_key).await,
  };
  if let Err(error) = freed {
    warn!(%error, "freeing an unfinished attachment's storage failed");
  }
}

fn storage_unavailable(error: StorageError) -> ApiError {
  warn!(%error, "attachment storage request failed");
  ApiError::storage_unavailable("attachment storage")
}

fn blob_store(state: &AppState) -> Result<&Arc<BlobStore>, ApiError> {
  state
    .maybe_blob_store
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("attachment storage is not configured".into()))
}

fn object_key(id: AttachmentId) -> String {
  format!("attachments/{id}")
}

fn expires_at(store: &BlobStore) -> UnixMillis {
  unix_millis(OffsetDateTime::now_utc() + store.expiry())
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{PresignAttachmentPartRequest, Snowflake, UploadedPart};

  use super::*;

  #[test]
  fn parts_are_limited_to_what_the_announced_size_needs() {
    const MIB: u64 = 1024 * 1024;
    let part = |part_number, size_bytes| PresignAttachmentPartRequest {
      part_number,
      size_bytes,
    };
    let size = 12 * MIB;
    assert_eq!(max_parts(size), 3);
    assert_eq!(max_parts(1), 1);
    assert_eq!(max_parts(u64::MAX), MAX_PARTS);
    assert!(check_part(size, &part(1, 8 * MIB)).is_ok());
    assert!(check_part(size, &part(2, 4 * MIB)).is_ok());
    assert!(check_part(size, &part(3, 2 * MIB)).is_ok());
    // Beyond the parts the size needs, more than what's left, empty, larger than a part may be.
    assert!(check_part(size, &part(4, 1)).is_err());
    assert!(check_part(size, &part(3, 2 * MIB + 1)).is_err());
    assert!(check_part(size, &part(1, 0)).is_err());
    assert!(check_part(64 * MIB, &part(1, MAX_PART_SIZE_BYTES + 1)).is_err());
  }

  #[test]
  fn parts_must_be_numbered_without_gaps() {
    let request = |numbers: &[u16]| CompleteMultipartAttachmentRequest {
      parts: numbers
        .iter()
        .map(|&part_number| UploadedPart {
          part_number,
          etag: format!("\"{part_number}\""),
        })
        .collect(),
    };
    assert_eq!(
      part_etags(&request(&[2, 1, 3])).unwrap(),
      ["\"1\"", "\"2\"", "\"3\""]
    );
    for bad in [&[][..], &[2], &[1, 3], &[1, 1], &[0, 1]] {
      assert!(part_etags(&request(bad)).is_err(), "{bad:?}");
    }
    let mut empty_etag = request(&[1]);
    empty_etag.parts[0].etag.clear();
    assert!(part_etags(&empty_etag).is_err());
  }

  #[test]
  fn object_keys_are_derived_from_the_attachment_id_only() {
    let id = AttachmentId::from_snowflake(Snowflake::new(42).unwrap());
    assert_eq!(object_key(id), "attachments/42");
  }
}
