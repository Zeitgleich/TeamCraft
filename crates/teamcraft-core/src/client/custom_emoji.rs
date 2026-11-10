//! The workspace's custom emoji on this device (issue #207, [`crate::custom_emoji`]): read on
//! connect and when the gateway says they changed, verified against the roster this device
//! verified, and their pictures downloaded in the background, a few at a time, so a large set
//! never holds up the sync. Admins add, rename and delete them by signing on this device.
//!
//! Every picture of the set is fetched, whether a message on screen uses it or not: what the
//! storage sees is "this device read the set", never which emoji a message names.

use futures_util::future::join_all;
use teamcraft_proto::custom_emoji_set::{CustomEmojiSet, NewShortcodeError};
use teamcraft_proto::{
  Base64Bytes, CreateCustomEmojiRequest, CustomEmojiDocument, CustomEmojiHash, CustomEmojiId,
  MAX_CUSTOM_EMOJI_BYTES_LIMIT, RenameCustomEmojiRequest, inspect_custom_emoji_picture,
};

use super::Client;
use crate::api::ServerApi;
use crate::custom_emoji::{
  has_picture, keep_picture, sign_custom_emoji, store_set, stored_set, verified_set,
};
use crate::error::CoreError;
use crate::store::LocalStore;

/// Pictures downloaded at once, and per round: the rest follow with later gateway frames.
const PICTURES_AT_ONCE: usize = 8;
const PICTURES_PER_ROUND: usize = 32;

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Reads the set, keeps what verifies and fetches pictures it lacks. Not fatal: a device that
  /// can't read it keeps the set it has, and an older server leaves it empty.
  pub(crate) async fn refresh_custom_emoji(&mut self) {
    let listed = match self.api.list_custom_emoji().await {
      Ok(listed) => listed,
      Err(error) if error.is_not_found() => teamcraft_proto::ListCustomEmojiResponse::default(),
      Err(error) => {
        tracing::debug!(%error, "could not read the custom emoji");
        return;
      },
    };
    let workspace = match self.workspace() {
      Ok(workspace) => workspace,
      Err(error) => {
        tracing::warn!(%error, "could not read the verified workspace for the custom emoji");
        return;
      },
    };
    let set = CustomEmojiSet {
      emoji: verified_set(&listed.emoji, &workspace),
      limits: listed.limits,
    };
    if let Err(error) = store_set(&self.store, &set) {
      tracing::warn!(%error, "could not store the custom emoji");
      return;
    }
    self.are_custom_emoji_pictures_due = true;
    Box::pin(self.fetch_custom_emoji_pictures()).await;
  }

  /// Reads the set again after a change, and goes on fetching pictures a round left.
  pub(super) async fn refresh_stale_custom_emoji(&mut self) {
    if self.is_custom_emoji_stale {
      self.is_custom_emoji_stale = false;
      Box::pin(self.refresh_custom_emoji()).await;
    } else if self.are_custom_emoji_pictures_due {
      Box::pin(self.fetch_custom_emoji_pictures()).await;
    }
  }

  /// Admins: signs and adds an emoji. The checks of the server run here first, so a refusal names
  /// the reason before anything is sent.
  pub async fn add_custom_emoji(&mut self, shortcode: String, picture: Vec<u8>) -> Result<(), CoreError> {
    let set = stored_set(&self.store)?;
    check_shortcode(&set, &shortcode, None)
      .map_err(|error| CoreError::InvalidRequest(shortcode_refusal(error).into()))?;
    let inspected = inspect_custom_emoji_picture(&picture, set.limits.max_bytes, set.limits.max_side_px)
      .map_err(|error| CoreError::InvalidRequest(error.to_string()))?;
    let sha256 = CustomEmojiHash::from_bytes(teamcraft_crypto::attachment::content_sha256(&picture));
    let document = CustomEmojiDocument {
      shortcode,
      sha256,
      format: inspected.format,
      signed_at_ms: self.now_ms(),
    };
    let signed = sign_custom_emoji(&self.account.device, &self.account.certificate, &document)?;
    self
      .api
      .create_custom_emoji(&CreateCustomEmojiRequest {
        picture: Base64Bytes::new(picture.clone()),
        signed,
      })
      .await?;
    // This device has the picture already; nothing to download.
    keep_picture(&self.store, &set, sha256, picture)?;
    Box::pin(self.refresh_custom_emoji()).await;
    Ok(())
  }

  /// Admins: a new name, signed over the same picture.
  pub async fn rename_custom_emoji(&mut self, id: CustomEmojiId, shortcode: String) -> Result<(), CoreError> {
    let set = stored_set(&self.store)?;
    let emoji = set
      .emoji
      .iter()
      .find(|emoji| emoji.id == id)
      .ok_or_else(|| CoreError::InvalidRequest("this custom emoji isn't in the verified set".into()))?;
    check_shortcode(&set, &shortcode, Some(id))
      .map_err(|error| CoreError::InvalidRequest(shortcode_refusal(error).into()))?;
    let document = CustomEmojiDocument {
      shortcode,
      sha256: emoji.sha256,
      format: emoji.format.clone(),
      signed_at_ms: self.now_ms(),
    };
    let signed = sign_custom_emoji(&self.account.device, &self.account.certificate, &document)?;
    self
      .api
      .rename_custom_emoji(id, &RenameCustomEmojiRequest { signed })
      .await?;
    Box::pin(self.refresh_custom_emoji()).await;
    Ok(())
  }

  /// Admins: deletes an emoji. Messages that use it keep showing it on devices that hold the
  /// picture, and the `:shortcode:` elsewhere.
  pub async fn delete_custom_emoji(&mut self, id: CustomEmojiId) -> Result<(), CoreError> {
    match self.api.delete_custom_emoji(id).await {
      Err(error) if error.is_not_found() => {},
      other => other?,
    }
    Box::pin(self.refresh_custom_emoji()).await;
    Ok(())
  }

  /// Downloads up to [`PICTURES_PER_ROUND`] pictures of the set this device lacks, checking each
  /// against its signed hash; leaves the flag up while more remain.
  async fn fetch_custom_emoji_pictures(&mut self) {
    let set = match stored_set(&self.store) {
      Ok(set) => set,
      Err(error) => {
        tracing::warn!(%error, "could not read the custom emoji");
        return;
      },
    };
    let missing: Vec<(CustomEmojiId, CustomEmojiHash)> = set
      .emoji
      .iter()
      .filter(|emoji| !has_picture(&self.store, emoji.sha256).unwrap_or(true))
      .map(|emoji| (emoji.id, emoji.sha256))
      .collect();
    self.are_custom_emoji_pictures_due = missing.len() > PICTURES_PER_ROUND;
    let max_bytes = set.limits.max_bytes.min(MAX_CUSTOM_EMOJI_BYTES_LIMIT);
    for batch in missing
      .chunks(PICTURES_AT_ONCE)
      .take(PICTURES_PER_ROUND / PICTURES_AT_ONCE)
    {
      let api = &self.api;
      let downloads = batch.iter().map(|(id, sha256)| async move {
        let download = api.download_custom_emoji(*id).await?;
        let bytes = api
          .get_blob_capped(&download.download_url, download.size_bytes.min(max_bytes))
          .await?;
        Ok::<_, crate::api::ApiError>((*sha256, bytes))
      });
      for result in join_all(downloads).await {
        match result {
          Ok((sha256, bytes)) => match keep_picture(&self.store, &set, sha256, bytes) {
            Ok(true) => {},
            Ok(false) => tracing::warn!("a custom emoji picture doesn't match its signed hash"),
            Err(error) => tracing::warn!(%error, "could not keep a custom emoji picture"),
          },
          Err(error) => tracing::debug!(%error, "could not download a custom emoji picture"),
        }
      }
    }
  }
}

/// [`CustomEmojiSet::check_new_shortcode`], and in builds with the UI also against the built-in
/// emoji, whose catalog only those have.
fn check_shortcode(
  set: &CustomEmojiSet,
  shortcode: &str,
  maybe_renamed: Option<CustomEmojiId>,
) -> Result<(), NewShortcodeError> {
  #[cfg(feature = "ui")]
  return teamcraft_client_api::custom_emoji::check_new_shortcode(set, shortcode, maybe_renamed);
  #[cfg(not(feature = "ui"))]
  set.check_new_shortcode(shortcode, maybe_renamed)
}

fn shortcode_refusal(error: NewShortcodeError) -> &'static str {
  match error {
    NewShortcodeError::Invalid => "a shortcode has 2 to 32 lower-case letters, digits, '_' or '-'",
    NewShortcodeError::BuiltIn => "a built-in emoji has this shortcode",
    NewShortcodeError::Taken => "another custom emoji has this shortcode",
  }
}
