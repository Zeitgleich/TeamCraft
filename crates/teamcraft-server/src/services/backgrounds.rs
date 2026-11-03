//! A user's own call background pictures. The client encrypts each picture with a fresh key and
//! keeps the key, name and list in its encrypted key bag (synced across the user's devices); the
//! server only hands out presigned URLs for the user's own objects and enforces the limits.

use std::sync::Arc;

use teamcraft_proto::{
  BackgroundDownloadResponse, BackgroundId, CreateBackgroundUploadRequest, CreateBackgroundUploadResponse,
  Id, UnixMillis,
};
use time::OffsetDateTime;
use tracing::warn;

use super::convert::unix_millis;

/// Extra time after an upload URL's expiry before a deleted background's row goes.
const SWEEP_MARGIN: time::Duration = time::Duration::minutes(5);
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;
use crate::storage::BlobStore;

pub async fn create_background_upload(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateBackgroundUploadRequest,
) -> Result<CreateBackgroundUploadResponse, ApiError> {
  let store = blob_store(state)?;
  // While the storage is down, say "later" here instead of handing out a URL that fails.
  store.check_available()?;
  let limits = &state.settings.get().limits;
  if request.size_bytes == 0 {
    return Err(ApiError::InvalidRequest("size_bytes must be positive".into()));
  }
  if request.size_bytes > limits.max_background_bytes {
    return Err(ApiError::PayloadTooLarge(format!(
      "backgrounds are limited to {} bytes",
      limits.max_background_bytes
    )));
  }
  if state.database.count_user_backgrounds(caller.user.id).await?
    >= u64::from(limits.max_backgrounds_per_user)
  {
    return Err(ApiError::Conflict(format!(
      "at most {} backgrounds; delete one first",
      limits.max_backgrounds_per_user
    )));
  }
  let id: BackgroundId = state.next_id()?;
  let key = format!("backgrounds/{}/{}", caller.user.id.snowflake(), id.snowflake());
  state
    .database
    .create_user_background(id, caller.user.id, &key, request.size_bytes)
    .await?;
  Ok(CreateBackgroundUploadResponse {
    id,
    upload_url: store.presign_upload(&key, request.size_bytes).to_string(),
    expires_at: expires_at(store),
  })
}

/// Confirms the upload: the stored object must have exactly the announced size.
pub async fn complete_background_upload(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: BackgroundId,
) -> Result<(), ApiError> {
  let store = blob_store(state)?;
  let background = own_background(state, caller, id).await?;
  if background.is_complete {
    return Ok(());
  }
  let stored = store.object_size(&background.s3_key).await.map_err(|error| {
    warn!(%error, "checking the uploaded background failed");
    ApiError::storage_unavailable("background storage")
  })?;
  match stored {
    None => Err(ApiError::Conflict("the background has not been uploaded".into())),
    Some(size) if size != background.size_bytes => Err(ApiError::Conflict(format!(
      "the uploaded background has {size} bytes, {} were announced",
      background.size_bytes
    ))),
    Some(_) => Ok(state.database.complete_user_background(id).await?),
  }
}

/// A download URL for one of the caller's own uploaded backgrounds.
pub async fn download_background(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: BackgroundId,
) -> Result<BackgroundDownloadResponse, ApiError> {
  let store = blob_store(state)?;
  let background = own_background(state, caller, id).await?;
  if !background.is_complete {
    return Err(ApiError::Conflict("the upload has not been completed".into()));
  }
  Ok(BackgroundDownloadResponse {
    download_url: store.presign_download(&background.s3_key).to_string(),
    size_bytes: background.size_bytes,
    expires_at: expires_at(store),
  })
}

/// Removes deleted and abandoned backgrounds (own and organization ones) whose upload URLs have
/// expired before `before`, deleting their objects once more: an upload URL handed out before a
/// delete keeps working until it expires, so an object can reappear after the first deletion.
pub async fn sweep(state: &AppState, before: OffsetDateTime) -> Result<usize, ApiError> {
  let Some(store) = state.maybe_blob_store.as_ref() else {
    return Ok(0);
  };
  if store.check_available().is_err() {
    // The rows go before their objects: while the storage is down the objects would stay behind
    // for good. The next sweep tries again.
    warn!("the storage is down; sweeping backgrounds later");
    return Ok(0);
  }
  let mut keys = state.database.sweep_user_backgrounds(before).await?;
  keys.extend(state.database.sweep_org_backgrounds(before).await?);
  for key in &keys {
    if let Err(error) = store.delete_object(key).await {
      warn!(%error, "deleting a swept background object failed");
    }
  }
  Ok(keys.len())
}

/// The cut-off for [`sweep`] now: upload URLs issued before it have expired (with a margin).
pub fn sweep_cutoff(state: &AppState, now: OffsetDateTime) -> OffsetDateTime {
  let expiry = state
    .maybe_blob_store
    .as_ref()
    .map_or(std::time::Duration::ZERO, |store| store.expiry());
  now - expiry - SWEEP_MARGIN
}

pub async fn delete_background(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: BackgroundId,
) -> Result<(), ApiError> {
  let store = blob_store(state)?;
  own_background(state, caller, id).await?;
  if let Some(key) = state.database.delete_user_background(id).await?
    && let Err(error) = store.delete_object(&key).await
  {
    // The row is gone, so nobody can fetch the object any more; storage cleanup can retry.
    warn!(%error, "deleting a background object failed");
  }
  Ok(())
}

/// The background if it belongs to the caller; others' look like missing ones.
async fn own_background(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: BackgroundId,
) -> Result<teamcraft_db::UserBackground, ApiError> {
  state
    .database
    .get_user_background(id)
    .await?
    .filter(|background| background.owner == caller.user.id)
    .ok_or_else(ApiError::background_not_found)
}

fn blob_store(state: &AppState) -> Result<&Arc<BlobStore>, ApiError> {
  state
    .maybe_blob_store
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("background storage is not configured".into()))
}

fn expires_at(store: &BlobStore) -> UnixMillis {
  unix_millis(OffsetDateTime::now_utc() + store.expiry())
}
