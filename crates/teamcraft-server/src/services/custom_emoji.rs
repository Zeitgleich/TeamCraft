//! Custom emoji of the workspace (issue #207, e2ee.md §4.12, admin.md §3.11). Admins add, rename
//! and delete them; everybody signed in lists them and downloads their pictures.
//!
//! The pictures are stored unencrypted, as sent: the document an admin's device signed names the
//! picture's SHA-256, and clients show a picture only when its signature verifies against the
//! signed admin roster and the bytes match. The server checks what it can without trusting
//! anybody's word: the picture's format, size and sides (the same check the clients run), the
//! name, that the document's hash and format are the picture's, and that the document claims the
//! caller as its signer. A message using an emoji names it inside its ciphertext; deleting an
//! emoji changes no message, it only takes the picture away for devices that don't hold it yet.

use std::sync::Arc;

use teamcraft_db::{CustomEmojiWrite, NewCustomEmoji, StoredCustomEmoji};
use teamcraft_proto::{
  CreateCustomEmojiRequest, CustomEmojiDocument, CustomEmojiDownloadResponse, CustomEmojiHash, CustomEmojiId,
  CustomEmojiLimits, CustomEmojiResponse, GatewayEvent, Id, ListCustomEmojiResponse, Permission,
  RenameCustomEmojiRequest, SignedCustomEmoji, Target, check_custom_emoji_shortcode,
  inspect_custom_emoji_picture,
};
use time::OffsetDateTime;
use tracing::warn;

use super::access::require;
use super::audit;
use super::convert::unix_millis;
use super::storage_cleanup::delete_or_queue;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;
use crate::storage::BlobStore;

/// Everybody signed in (guests too: they read the messages that use the emoji).
pub async fn list(state: &AppState) -> Result<ListCustomEmojiResponse, ApiError> {
  let emoji = state
    .database
    .list_custom_emoji()
    .await?
    .into_iter()
    .map(response)
    .collect();
  Ok(ListCustomEmojiResponse {
    emoji,
    limits: limits(state),
  })
}

/// Admins: adds an emoji with its signed document.
pub async fn create(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateCustomEmojiRequest,
) -> Result<CustomEmojiResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let limits = limits(state);
  let picture = request.picture.as_slice();
  if picture.len() as u64 > limits.max_bytes {
    return Err(ApiError::PayloadTooLarge(format!(
      "custom emoji pictures are at most {} bytes",
      limits.max_bytes
    )));
  }
  let inspected = inspect_custom_emoji_picture(picture, limits.max_bytes, limits.max_side_px)
    .map_err(|error| ApiError::InvalidRequest(error.to_string()))?;
  let document = checked_document(caller, &request.signed)?;
  let sha256 = CustomEmojiHash::from_bytes(teamcraft_crypto::attachment::content_sha256(picture));
  if document.sha256 != sha256 || document.format != inspected.format {
    return Err(ApiError::InvalidRequest(
      "the signed document names another picture".into(),
    ));
  }
  if state.database.count_custom_emoji().await? >= u64::from(limits.max_count) {
    return Err(ApiError::Conflict(format!(
      "at most {} custom emoji; delete one first",
      limits.max_count
    )));
  }
  let store = blob_store(state)?;
  let id: CustomEmojiId = state.next_id()?;
  let key = format!("custom-emoji/{}", id.snowflake());
  store
    .put_object(&key, picture.to_vec(), inspected.format.as_str())
    .await
    .map_err(|error| {
      warn!(%error, "storing a custom emoji failed");
      ApiError::storage_failed("custom emoji storage", error)
    })?;
  let new = NewCustomEmoji {
    id,
    shortcode: &document.shortcode,
    sha256,
    format: &inspected.format,
    s3_key: &key,
    size_bytes: picture.len() as u64,
    width: inspected.width,
    height: inspected.height,
    payload: request.signed.payload.as_slice(),
    signature: request.signed.signature.as_slice(),
    created_by: caller.user.id,
  };
  let written = match state.database.create_custom_emoji(&new).await {
    Ok(written) => written,
    Err(error) => {
      delete_or_queue(state, store, &key).await;
      return Err(error.into());
    },
  };
  if written == CustomEmojiWrite::NameTaken {
    delete_or_queue(state, store, &key).await;
    return Err(ApiError::CustomEmojiNameTaken);
  }
  audit::record(
    state,
    Some(caller.user.id),
    "custom_emoji_added",
    None,
    &serde_json::json!({"id": id, "shortcode": document.shortcode}),
  )
  .await?;
  announce(state);
  let stored = state
    .database
    .get_custom_emoji(id)
    .await?
    .ok_or_else(ApiError::custom_emoji_not_found)?;
  Ok(response(stored))
}

/// Admins: a new name, with a document signed over the same picture.
pub async fn rename(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: CustomEmojiId,
  request: RenameCustomEmojiRequest,
) -> Result<CustomEmojiResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let current = state
    .database
    .get_custom_emoji(id)
    .await?
    .ok_or_else(ApiError::custom_emoji_not_found)?;
  let document = checked_document(caller, &request.signed)?;
  if document.sha256 != current.sha256 || document.format != current.format {
    return Err(ApiError::InvalidRequest(
      "a new name is signed over the same picture".into(),
    ));
  }
  let written = state
    .database
    .rename_custom_emoji(
      id,
      &document.shortcode,
      request.signed.payload.as_slice(),
      request.signed.signature.as_slice(),
    )
    .await?;
  match written {
    CustomEmojiWrite::Done => {},
    CustomEmojiWrite::NameTaken => return Err(ApiError::CustomEmojiNameTaken),
    CustomEmojiWrite::NotFound => return Err(ApiError::custom_emoji_not_found()),
  }
  audit::record(
    state,
    Some(caller.user.id),
    "custom_emoji_renamed",
    None,
    &serde_json::json!({"id": id, "from": current.shortcode, "to": document.shortcode}),
  )
  .await?;
  announce(state);
  let stored = state
    .database
    .get_custom_emoji(id)
    .await?
    .ok_or_else(ApiError::custom_emoji_not_found)?;
  Ok(response(stored))
}

/// Admins: deletes an emoji and its picture. Messages that use it stay as they are: devices that
/// hold the picture keep showing it, the others show the `:shortcode:`.
pub async fn delete(state: &AppState, caller: &AuthenticatedUser, id: CustomEmojiId) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let store = blob_store(state)?;
  let key = state
    .database
    .delete_custom_emoji(id)
    .await?
    .ok_or_else(ApiError::custom_emoji_not_found)?;
  delete_or_queue(state, store, &key).await;
  audit::record(
    state,
    Some(caller.user.id),
    "custom_emoji_deleted",
    None,
    &serde_json::json!({"id": id}),
  )
  .await?;
  announce(state);
  Ok(())
}

/// A download URL of the picture, for everybody signed in.
pub async fn download(state: &AppState, id: CustomEmojiId) -> Result<CustomEmojiDownloadResponse, ApiError> {
  let store = blob_store(state)?;
  let emoji = state
    .database
    .get_custom_emoji(id)
    .await?
    .ok_or_else(ApiError::custom_emoji_not_found)?;
  Ok(CustomEmojiDownloadResponse {
    download_url: store.presign_download(&emoji.s3_key).to_string(),
    size_bytes: emoji.size_bytes,
    expires_at: unix_millis(OffsetDateTime::now_utc() + store.expiry()),
  })
}

/// The instance's limits for new emoji.
pub fn limits(state: &AppState) -> CustomEmojiLimits {
  let limits = &state.settings.get().limits;
  CustomEmojiLimits {
    max_count: limits.max_custom_emoji,
    max_bytes: limits.max_custom_emoji_bytes,
    max_side_px: limits.max_custom_emoji_side_px,
  }
}

/// The signed document, read: a valid name, and the caller as the signer it claims. The signature
/// itself is the members' devices' to verify, against the admin roster they verified.
fn checked_document(
  caller: &AuthenticatedUser,
  signed: &SignedCustomEmoji,
) -> Result<CustomEmojiDocument, ApiError> {
  let document: CustomEmojiDocument = serde_json::from_slice(signed.payload.as_slice())
    .map_err(|_| ApiError::InvalidRequest("the signed document can't be read".into()))?;
  check_custom_emoji_shortcode(&document.shortcode)
    .map_err(|error| ApiError::InvalidRequest(error.to_string()))?;
  let signer = teamcraft_crypto::workspace::claimed_signer(signed.signature.as_slice())
    .map_err(|_| ApiError::InvalidRequest("the signature can't be read".into()))?;
  if i64::try_from(signer.get()).ok() != Some(caller.user.id.snowflake().get()) {
    return Err(ApiError::InvalidRequest(
      "the document is signed by somebody else".into(),
    ));
  }
  Ok(document)
}

fn response(emoji: StoredCustomEmoji) -> CustomEmojiResponse {
  CustomEmojiResponse {
    id: emoji.id,
    shortcode: emoji.shortcode,
    sha256: emoji.sha256,
    format: emoji.format,
    size_bytes: emoji.size_bytes,
    width: emoji.width,
    height: emoji.height,
    maybe_created_by: emoji.maybe_created_by,
    created_at: unix_millis(emoji.created_at),
    signed: SignedCustomEmoji {
      payload: emoji.payload.into(),
      signature: emoji.signature.into(),
    },
  }
}

/// Tells everybody online to read the list again.
fn announce(state: &AppState) {
  state.hub.publish_all(&GatewayEvent::CustomEmojiUpdate);
}

fn blob_store(state: &AppState) -> Result<&Arc<BlobStore>, ApiError> {
  state
    .maybe_blob_store
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("custom emoji storage is not configured".into()))
}
