//! Attachments (e2ee.md §4.5): clients encrypt, upload and download blobs through presigned URLs.
//! The server records who may fetch a blob and checks the uploaded size; keys and names stay in
//! the encrypted message.

use std::sync::Arc;

use teamcraft_db::NewAttachment;
use teamcraft_proto::{
  AttachmentDownloadResponse, AttachmentId, AttachmentResponse, CreateAttachmentRequest,
  CreateAttachmentResponse, HistoryPolicy, Id, Permission, UnixMillis,
};
use time::OffsetDateTime;
use tracing::warn;

use super::access::{is_guest, load_conversation, require};
use super::convert::unix_millis;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;
use crate::storage::BlobStore;

pub async fn create_attachment(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateAttachmentRequest,
) -> Result<CreateAttachmentResponse, ApiError> {
  let store = blob_store(state)?;
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
    })
    .await?;
  Ok(CreateAttachmentResponse {
    id,
    upload_url: store.presign_upload(&key, request.size_bytes).to_string(),
    expires_at: expires_at(store),
  })
}

/// Confirms the upload: the stored object must have exactly the announced size.
pub async fn complete_attachment(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: AttachmentId,
) -> Result<AttachmentResponse, ApiError> {
  let store = blob_store(state)?;
  let attachment = state
    .database
    .get_attachment(id)
    .await?
    .ok_or_else(ApiError::attachment_not_found)?;
  if attachment.maybe_uploader != Some(caller.user.id) {
    return Err(ApiError::attachment_not_found());
  }
  if !attachment.is_complete {
    let stored = store.object_size(&attachment.s3_key).await.map_err(|error| {
      warn!(%error, "checking the uploaded object failed");
      ApiError::Unavailable("attachment storage".into())
    })?;
    match stored {
      None => return Err(ApiError::Conflict("the blob has not been uploaded".into())),
      Some(size) if size != attachment.size_bytes => {
        return Err(ApiError::Conflict(format!(
          "the uploaded blob has {size} bytes, {} were announced",
          attachment.size_bytes
        )));
      },
      Some(_) => state.database.complete_attachment(id).await?,
    }
  }
  Ok(AttachmentResponse {
    id,
    conversation_id: attachment
      .maybe_conversation
      .ok_or_else(ApiError::attachment_not_found)?,
    size_bytes: attachment.size_bytes,
    is_complete: true,
  })
}

/// A download URL for members of the attachment's conversation (guests and `from_join` channels:
/// only for attachments created after joining).
pub async fn download_attachment(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: AttachmentId,
) -> Result<AttachmentDownloadResponse, ApiError> {
  let store = blob_store(state)?;
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
  use teamcraft_proto::Snowflake;

  use super::*;

  #[test]
  fn object_keys_are_derived_from_the_attachment_id_only() {
    let id = AttachmentId::from_snowflake(Snowflake::new(42).unwrap());
    assert_eq!(object_key(id), "attachments/42");
  }
}
