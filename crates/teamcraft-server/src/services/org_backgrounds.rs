//! Call background pictures for the organization (admin.md §3.5). Admins upload them; they are
//! stored unencrypted and readable by everybody in the workspace, because every call participant
//! sees them anyway. Owners list them, with each picture's SHA-256, in the signed workspace
//! policy; clients check the hash before using a picture, so the server can't swap one.

use std::sync::Arc;

use teamcraft_proto::{
  CreateOrgBackgroundUploadRequest, CreateOrgBackgroundUploadResponse, Id, OrgBackgroundDownloadResponse,
  OrgBackgroundId, Permission, Target, UnixMillis,
};
use time::OffsetDateTime;
use tracing::warn;

use super::access::require;
use super::audit;
use super::convert::unix_millis;
use super::workspace::MAX_ORG_BACKGROUNDS;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;
use crate::storage::BlobStore;

pub async fn create_upload(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateOrgBackgroundUploadRequest,
) -> Result<CreateOrgBackgroundUploadResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let store = blob_store(state)?;
  let max_bytes = state.settings.get().limits.max_background_bytes;
  if request.size_bytes == 0 {
    return Err(ApiError::InvalidRequest("size_bytes must be positive".into()));
  }
  if request.size_bytes > max_bytes {
    return Err(ApiError::PayloadTooLarge(format!(
      "backgrounds are limited to {max_bytes} bytes"
    )));
  }
  if state.database.count_org_backgrounds().await? >= MAX_ORG_BACKGROUNDS as u64 {
    return Err(ApiError::Conflict(format!(
      "at most {MAX_ORG_BACKGROUNDS} organization backgrounds; delete one first"
    )));
  }
  let id: OrgBackgroundId = state.next_id()?;
  let key = format!("org-backgrounds/{}", id.snowflake());
  state
    .database
    .create_org_background(id, caller.user.id, &key, request.size_bytes)
    .await?;
  Ok(CreateOrgBackgroundUploadResponse {
    id,
    upload_url: store.presign_upload(&key, request.size_bytes).to_string(),
    expires_at: expires_at(store),
  })
}

/// Confirms the upload: the stored object must have exactly the announced size.
pub async fn complete_upload(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: OrgBackgroundId,
) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let store = blob_store(state)?;
  let background = state
    .database
    .get_org_background(id)
    .await?
    .ok_or_else(ApiError::background_not_found)?;
  if background.is_complete {
    return Ok(());
  }
  let stored = store.object_size(&background.s3_key).await.map_err(|error| {
    warn!(%error, "checking an uploaded organization background failed");
    ApiError::Unavailable("background storage".into())
  })?;
  match stored {
    None => Err(ApiError::Conflict("the background has not been uploaded".into())),
    Some(size) if size != background.size_bytes => Err(ApiError::Conflict(format!(
      "the uploaded background has {size} bytes, {} were announced",
      background.size_bytes
    ))),
    Some(_) => {
      state.database.complete_org_background(id).await?;
      audit::record(
        state,
        Some(caller.user.id),
        "org_background_uploaded",
        None,
        &serde_json::json!({"id": id}),
      )
      .await?;
      Ok(())
    },
  }
}

/// A download URL for everybody in the workspace (guests included).
pub async fn download(
  state: &AppState,
  id: OrgBackgroundId,
) -> Result<OrgBackgroundDownloadResponse, ApiError> {
  let store = blob_store(state)?;
  let background = state
    .database
    .get_org_background(id)
    .await?
    .filter(|background| background.is_complete)
    .ok_or_else(ApiError::background_not_found)?;
  Ok(OrgBackgroundDownloadResponse {
    download_url: store.presign_download(&background.s3_key).to_string(),
    size_bytes: background.size_bytes,
    expires_at: expires_at(store),
  })
}

/// Deletes a picture the current policy no longer lists.
pub async fn delete(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: OrgBackgroundId,
) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let store = blob_store(state)?;
  if state
    .policy
    .get()
    .call_backgrounds
    .images
    .iter()
    .any(|image| image.id == id)
  {
    return Err(ApiError::Conflict(
      "the workspace policy still lists this background; remove it there first".into(),
    ));
  }
  let key = state
    .database
    .delete_org_background(id)
    .await?
    .ok_or_else(ApiError::background_not_found)?;
  if let Err(error) = store.delete_object(&key).await {
    // The row is gone, so nobody gets a URL for it any more; storage cleanup can retry.
    warn!(%error, "deleting an organization background object failed");
  }
  audit::record(
    state,
    Some(caller.user.id),
    "org_background_deleted",
    None,
    &serde_json::json!({"id": id}),
  )
  .await?;
  Ok(())
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
