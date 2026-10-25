//! Channel pictures. Like the channel's name they are directory data the server sees, not
//! end-to-end encrypted content (e2ee.md §4.4), handled exactly like space pictures
//! ([`super::space_images`]): decoded and re-encoded as PNG here, stored in the blob store under a
//! fresh key per picture. Only people with `manage_channel_properties` set or remove it; everybody
//! who may see the channel may download it.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use serde_json::json;
use teamcraft_db::StoredSpaceImage;
use teamcraft_proto::{
  AttachmentId, ConversationId, ConversationKind, ConversationResponse, GatewayEvent, Id, Permission,
};
use tracing::warn;

use super::access::{load_conversation, require};
use super::audit::{self, Target as AuditTarget};
use super::conversations::get_conversation;
use super::space_images::{SPACE_IMAGE_CONTENT_TYPE, SpaceImageFile, run_limited, sanitize};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;
use crate::storage::BlobStore;

/// Replaces the channel's picture with the uploaded image.
pub async fn put_channel_image(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
  declared_type: &str,
  bytes: Bytes,
) -> Result<ConversationResponse, ApiError> {
  let access = load_conversation(state, caller, id).await?;
  ensure_channel(&access.view.conversation.kind)?;
  require(
    state,
    caller,
    Permission::ManageChannelProperties,
    &access.target(),
  )?;
  let (max_bytes, timeout) = {
    let limits = &state.settings.get().limits;
    (
      limits.max_space_image_bytes,
      Duration::from_secs(limits.image_decode_timeout_secs),
    )
  };
  if bytes.is_empty() {
    return Err(ApiError::InvalidRequest("the picture is empty".into()));
  }
  if bytes.len() as u64 > max_bytes {
    return Err(ApiError::PayloadTooLarge(format!(
      "channel pictures are at most {max_bytes} bytes"
    )));
  }
  let declared_type = declared_type.to_owned();
  let png = run_limited(&state.image_decodes, timeout, move || {
    sanitize(&bytes, &declared_type)
  })
  .await?
  .map_err(|error| ApiError::InvalidRequest(error.to_string()))?;
  if png.len() as u64 > max_bytes {
    return Err(ApiError::PayloadTooLarge(format!(
      "the re-encoded picture is larger than {max_bytes} bytes"
    )));
  }
  let store = blob_store(state)?;
  // A fresh key per picture: a reader never gets a half-replaced object.
  let object: AttachmentId = state.next_id()?;
  let image = StoredSpaceImage {
    key: format!("channel-images/{}/{}", id.snowflake(), object.snowflake()),
    version: teamcraft_crypto::account::subresource_integrity(&png),
    size_bytes: png.len() as u64,
  };
  store
    .put_object(&image.key, png, SPACE_IMAGE_CONTENT_TYPE)
    .await
    .map_err(|error| ApiError::Unavailable(format!("storing the picture failed: {error}")))?;
  let replaced = match state.database.set_conversation_image(id, Some(&image)).await {
    Ok(replaced) => replaced,
    Err(error) => {
      forget_object(store, &image.key).await;
      return Err(error.into());
    },
  };
  if let Some(previous) = replaced {
    forget_object(store, &previous).await;
  }
  announce(state, caller, id, "channel_image_set").await?;
  get_conversation(state, caller, id).await
}

/// Removes the channel's picture.
pub async fn delete_channel_image(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
) -> Result<ConversationResponse, ApiError> {
  let access = load_conversation(state, caller, id).await?;
  ensure_channel(&access.view.conversation.kind)?;
  require(
    state,
    caller,
    Permission::ManageChannelProperties,
    &access.target(),
  )?;
  if access.view.conversation.maybe_image.is_none() {
    return Err(ApiError::channel_image_not_found());
  }
  let store = blob_store(state)?;
  if let Some(previous) = state.database.set_conversation_image(id, None).await? {
    forget_object(store, &previous).await;
  }
  announce(state, caller, id, "channel_image_removed").await?;
  get_conversation(state, caller, id).await
}

/// The picture of a channel the caller may see.
pub async fn get_channel_image(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
) -> Result<SpaceImageFile, ApiError> {
  let access = load_conversation(state, caller, id).await?;
  let image = access
    .view
    .conversation
    .maybe_image
    .ok_or_else(ApiError::channel_image_not_found)?;
  let store = blob_store(state)?;
  let bytes = store
    .get_object(&image.key, image.size_bytes)
    .await
    .map_err(|error| {
      warn!(%error, conversation_id = %id, "reading a channel picture failed");
      ApiError::Unavailable("picture storage".into())
    })?
    .ok_or_else(ApiError::channel_image_not_found)?;
  Ok(SpaceImageFile {
    bytes,
    version: image.version,
  })
}

/// Only channels have pictures: DMs show their participants' avatars.
fn ensure_channel(kind: &ConversationKind) -> Result<(), ApiError> {
  if *kind == ConversationKind::Channel {
    Ok(())
  } else {
    Err(ApiError::InvalidRequest("only channels have pictures".into()))
  }
}

async fn announce(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
  action: &str,
) -> Result<(), ApiError> {
  audit::record(
    state,
    Some(caller.user.id),
    action,
    Some(AuditTarget::new("conversation", id)),
    &json!({}),
  )
  .await?;
  let members = state.database.conversation_member_ids(id).await?;
  state
    .hub
    .publish(members, &GatewayEvent::ConversationUpdate { conversation_id: id });
  Ok(())
}

fn blob_store(state: &AppState) -> Result<&Arc<BlobStore>, ApiError> {
  state
    .maybe_blob_store
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("storage is not configured".into()))
}

/// Deletes a picture nobody refers to any more; a failure only leaves an orphan behind.
async fn forget_object(store: &BlobStore, key: &str) {
  if let Err(error) = store.delete_object(key).await {
    warn!(%error, "deleting an old channel picture failed");
  }
}
