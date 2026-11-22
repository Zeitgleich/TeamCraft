//! Sticker packs of the workspace (issue #2132, e2ee.md §4.14, admin.md §3.11). Admins upload
//! pictures and save packs; everybody signed in lists the packs and downloads their pictures.
//!
//! As for custom emoji, the pictures are stored unencrypted as sent, keyed by their SHA-256, and the
//! document an admin's device signed names them: clients use a pack only when its signature verifies
//! against the signed admin roster, and a picture only when its bytes match. The server checks what
//! it can without trusting anybody's word: each picture's format, size and sides (the same check the
//! clients run), the document's rules, that every picture it names is held with that format, that
//! its version is newer than the one held, and that it claims the caller as its signer. A sent
//! sticker names its picture inside the ciphertext; deleting a pack changes no message.

use std::sync::Arc;

use teamcraft_db::{NewStickerPicture, SaveStickerPack, StickerPackWrite, StoredStickerPack};
use teamcraft_proto::{
  CustomEmojiHash, GatewayEvent, Id, ListStickerPacksResponse, Permission, SaveStickerPackRequest,
  SignedStickerPack, StickerLimits, StickerPackDocument, StickerPackId, StickerPackKey, StickerPackResponse,
  StickerPictureDownloadResponse, StickerPictureId, StickerPictureResponse, Target,
  UploadStickerPictureRequest, inspect_custom_emoji_picture,
};
use time::{Duration, OffsetDateTime};
use tracing::warn;

use super::access::require;
use super::audit;
use super::convert::unix_millis;
use super::storage_cleanup::delete_or_queue;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;
use crate::storage::BlobStore;

/// How long a picture no pack names is kept: long enough for an admin to upload a pack's pictures
/// and save it.
const UNUSED_PICTURE_GRACE: Duration = Duration::hours(24);

/// Everybody signed in (guests too: they read the messages that use the stickers).
pub async fn list(state: &AppState) -> Result<ListStickerPacksResponse, ApiError> {
  let packs = state
    .database
    .list_sticker_packs()
    .await?
    .into_iter()
    .map(response)
    .collect();
  Ok(ListStickerPacksResponse {
    packs,
    limits: limits(state),
  })
}

/// Admins: keeps a picture for packs. One the server holds already is answered as it is.
pub async fn upload_picture(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: UploadStickerPictureRequest,
) -> Result<StickerPictureResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let limits = limits(state);
  let picture = request.picture.as_slice();
  if picture.len() as u64 > limits.max_bytes {
    return Err(ApiError::PayloadTooLarge(format!(
      "sticker pictures are at most {} bytes",
      limits.max_bytes
    )));
  }
  let inspected = inspect_custom_emoji_picture(picture, limits.max_bytes, limits.max_side_px)
    .map_err(|error| ApiError::InvalidRequest(error.to_string()))?;
  let sha256 = CustomEmojiHash::from_bytes(teamcraft_crypto::attachment::content_sha256(picture));
  if let Some(held) = state.database.get_sticker_picture(sha256).await? {
    return Ok(picture_response(&held));
  }
  let store = blob_store(state)?;
  let id: StickerPictureId = state.next_id()?;
  let key = format!("sticker-pictures/{}", sha256.to_hex());
  store
    .put_object(&key, picture.to_vec(), inspected.format.as_str())
    .await
    .map_err(|error| {
      warn!(%error, "storing a sticker picture failed");
      ApiError::storage_failed("sticker storage", error)
    })?;
  let new = NewStickerPicture {
    id,
    sha256,
    format: &inspected.format,
    s3_key: &key,
    size_bytes: picture.len() as u64,
    width: inspected.width,
    height: inspected.height,
    uploaded_by: caller.user.id,
  };
  // Of two uploads of one picture, the second finds the row of the first; both put the same bytes
  // under the same key, so the object stays.
  state.database.create_sticker_picture(&new).await?;
  let held = state
    .database
    .get_sticker_picture(sha256)
    .await?
    .ok_or_else(ApiError::sticker_pack_not_found)?;
  Ok(picture_response(&held))
}

/// Admins: creates a pack or replaces it with a newer version of its signed document.
pub async fn save(
  state: &AppState,
  caller: &AuthenticatedUser,
  pack: StickerPackKey,
  request: SaveStickerPackRequest,
) -> Result<StickerPackResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let limits = limits(state);
  let document = checked_document(caller, &request.signed, limits)?;
  if document.pack != pack {
    return Err(ApiError::InvalidRequest(
      "the signed document names another pack".into(),
    ));
  }
  let mut pictures: Vec<CustomEmojiHash> = Vec::with_capacity(document.stickers.len());
  for sticker in &document.stickers {
    if pictures.contains(&sticker.sha256) {
      continue;
    }
    let held = state
      .database
      .get_sticker_picture(sticker.sha256)
      .await?
      .ok_or_else(|| ApiError::InvalidRequest("a sticker's picture isn't uploaded".into()))?;
    if held.format != sticker.format {
      return Err(ApiError::InvalidRequest(
        "a sticker's format isn't its picture's".into(),
      ));
    }
    pictures.push(sticker.sha256);
  }
  let is_new = state.database.get_sticker_pack(pack).await?.is_none();
  if is_new && state.database.count_sticker_packs().await? >= u64::from(limits.max_packs) {
    return Err(ApiError::Conflict(format!(
      "at most {} sticker packs; delete one first",
      limits.max_packs
    )));
  }
  let new_id: StickerPackId = state.next_id()?;
  let written = state
    .database
    .save_sticker_pack(&SaveStickerPack {
      new_id,
      pack,
      version: document.version,
      name: &document.name,
      payload: request.signed.payload.as_slice(),
      signature: request.signed.signature.as_slice(),
      updated_by: caller.user.id,
      pictures: &pictures,
    })
    .await?;
  let action = match written {
    StickerPackWrite::Created => "sticker_pack_added",
    StickerPackWrite::Updated => "sticker_pack_changed",
    StickerPackWrite::Stale => return Err(ApiError::StickerPackStale),
    // Deleted as unused between the check and the save.
    StickerPackWrite::MissingPicture => {
      return Err(ApiError::InvalidRequest(
        "a sticker's picture isn't uploaded".into(),
      ));
    },
  };
  audit::record(
    state,
    Some(caller.user.id),
    action,
    None,
    &serde_json::json!({"pack": pack, "version": document.version, "stickers": document.stickers.len()}),
  )
  .await?;
  delete_unused_pictures(state).await;
  announce(state);
  let stored = state
    .database
    .get_sticker_pack(pack)
    .await?
    .ok_or_else(ApiError::sticker_pack_not_found)?;
  Ok(response(stored))
}

/// Admins: deletes a pack. Messages that sent its stickers stay as they are: devices that hold the
/// pictures keep showing them, the others show the fallback text.
pub async fn delete(
  state: &AppState,
  caller: &AuthenticatedUser,
  pack: StickerPackKey,
) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  if !state.database.delete_sticker_pack(pack).await? {
    return Err(ApiError::sticker_pack_not_found());
  }
  audit::record(
    state,
    Some(caller.user.id),
    "sticker_pack_deleted",
    None,
    &serde_json::json!({"pack": pack}),
  )
  .await?;
  delete_unused_pictures(state).await;
  announce(state);
  Ok(())
}

/// A download URL of a picture, for everybody signed in.
pub async fn download_picture(
  state: &AppState,
  sha256: CustomEmojiHash,
) -> Result<StickerPictureDownloadResponse, ApiError> {
  let store = blob_store(state)?;
  let picture = state
    .database
    .get_sticker_picture(sha256)
    .await?
    .ok_or_else(ApiError::sticker_pack_not_found)?;
  Ok(StickerPictureDownloadResponse {
    download_url: store.presign_download(&picture.s3_key).to_string(),
    size_bytes: picture.size_bytes,
    expires_at: unix_millis(OffsetDateTime::now_utc() + store.expiry()),
  })
}

/// The instance's limits for packs.
pub fn limits(state: &AppState) -> StickerLimits {
  let limits = &state.settings.get().limits;
  StickerLimits {
    max_packs: limits.max_sticker_packs,
    max_stickers_per_pack: limits.max_stickers_per_pack,
    max_bytes: limits.max_sticker_bytes,
    max_side_px: limits.max_sticker_side_px,
  }
}

/// The signed document, read: its rules within the limits, and the caller as the signer it claims.
/// The signature itself is the members' devices' to verify, against the admin roster they verified.
fn checked_document(
  caller: &AuthenticatedUser,
  signed: &SignedStickerPack,
  limits: StickerLimits,
) -> Result<StickerPackDocument, ApiError> {
  let document: StickerPackDocument = serde_json::from_slice(signed.payload.as_slice())
    .map_err(|_| ApiError::InvalidRequest("the signed document can't be read".into()))?;
  document
    .check(limits.max_stickers_per_pack)
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

/// Deletes pictures no pack names past [`UNUSED_PICTURE_GRACE`]. A failure leaves them for the next
/// save or delete.
async fn delete_unused_pictures(state: &AppState) {
  let Ok(store) = blob_store(state) else {
    return;
  };
  match state
    .database
    .delete_unused_sticker_pictures(OffsetDateTime::now_utc() - UNUSED_PICTURE_GRACE)
    .await
  {
    Ok(keys) => {
      for key in keys {
        delete_or_queue(state, store, &key).await;
      }
    },
    Err(error) => warn!(%error, "could not delete unused sticker pictures"),
  }
}

fn response(pack: StoredStickerPack) -> StickerPackResponse {
  StickerPackResponse {
    pack: pack.pack,
    version: pack.version,
    name: pack.name,
    maybe_updated_by: pack.maybe_updated_by,
    updated_at: unix_millis(pack.updated_at),
    signed: SignedStickerPack {
      payload: pack.payload.into(),
      signature: pack.signature.into(),
    },
  }
}

fn picture_response(picture: &teamcraft_db::StoredStickerPicture) -> StickerPictureResponse {
  StickerPictureResponse {
    sha256: picture.sha256,
    format: picture.format.clone(),
    size_bytes: picture.size_bytes,
    width: picture.width,
    height: picture.height,
  }
}

/// Tells everybody online to read the packs again.
fn announce(state: &AppState) {
  state.hub.publish_all(&GatewayEvent::StickerPacksUpdate);
}

fn blob_store(state: &AppState) -> Result<&Arc<BlobStore>, ApiError> {
  state
    .maybe_blob_store
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("sticker storage is not configured".into()))
}
