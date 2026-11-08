//! Deletes that must not get lost (issue #743). The rows of removed objects go first, so nobody can
//! fetch them any more; a delete that then fails would leave the object behind for good, with
//! nothing remembering it. It waits in a queue in the database for the next sweep instead.

use tracing::{info, warn};

use crate::error::ApiError;
use crate::state::AppState;
use crate::storage::{BlobStore, StorageError};

/// Objects looked at per sweep; a longer queue goes on at the next one.
const SWEEP_BATCH: i64 = 500;

/// Deletes `key`, or queues it for the next sweep.
pub(crate) async fn delete_or_queue(state: &AppState, store: &BlobStore, key: &str) {
  queue_on_failure(state, key, None, store.delete_object(key).await).await;
}

/// Aborts the upload in parts `upload_id` of `key`, or queues it for the next sweep.
pub(crate) async fn abort_or_queue(state: &AppState, store: &BlobStore, key: &str, upload_id: &str) {
  queue_on_failure(
    state,
    key,
    Some(upload_id),
    store.abort_multipart(key, upload_id).await,
  )
  .await;
}

/// Works off the queue; stops at the first failure, the storage is down again. Returns how many
/// objects were deleted.
pub async fn sweep(state: &AppState) -> Result<usize, ApiError> {
  let Some(store) = state.maybe_blob_store.as_ref() else {
    return Ok(0);
  };
  if store.check_available().is_err() {
    return Ok(0);
  }
  let mut deleted = 0;
  for cleanup in state.database.storage_cleanups(SWEEP_BATCH).await? {
    let freed = free(
      store,
      &cleanup.s3_key,
      cleanup.maybe_multipart_upload_id.as_deref(),
    )
    .await;
    if let Err(error) = freed {
      warn!(%error, "deleting a queued object failed; trying again at the next sweep");
      break;
    }
    state.database.remove_storage_cleanup(&cleanup.s3_key).await?;
    deleted += 1;
  }
  if deleted > 0 {
    info!(deleted, "deleted objects that waited for the storage");
  }
  Ok(deleted)
}

async fn free(store: &BlobStore, key: &str, maybe_upload_id: Option<&str>) -> Result<(), StorageError> {
  match maybe_upload_id {
    Some(upload_id) => store.abort_multipart(key, upload_id).await,
    None => store.delete_object(key).await,
  }
}

async fn queue_on_failure(
  state: &AppState,
  key: &str,
  maybe_upload_id: Option<&str>,
  outcome: Result<(), StorageError>,
) {
  let Err(error) = outcome else {
    return;
  };
  warn!(%error, "deleting an object failed; it waits for the next sweep");
  if let Err(error) = state.database.queue_storage_cleanup(key, maybe_upload_id).await {
    warn!(%error, "an object that could not be deleted could not be queued either");
  }
}
