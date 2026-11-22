//! The workspace's sticker packs on this device (issue #2132, [`crate::stickers`]): read when the UI
//! needs them (`CoreChatBackend::want_sticker_packs`: the sticker picker, a sticker a message shows
//! and this device lacks), at most once a minute and again after the gateway said they or the
//! roster changed. Verified against the roster this device verified; their pictures are downloaded
//! a round at a time. Admins change a pack by signing its next version on this device.
//!
//! As for custom emoji, every picture of the set is fetched, whether a message on screen uses it or
//! not: what the storage sees is "this device read the packs", never which sticker a message names.

use rand_core::{OsRng, RngCore};
use teamcraft_proto::{
  Base64Bytes, CustomEmojiHash, MAX_STICKER_BYTES_LIMIT, SaveStickerPackRequest, StickerEntry,
  StickerPackChange, StickerPackDocument, StickerPackKey, StickerRef, StickerSet,
  UploadStickerPictureRequest, check_custom_emoji_shortcode, check_sticker_alt, inspect_custom_emoji_picture,
};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::stickers::{has_picture, keep_picture, sign_sticker_pack, store_set, stored_set, verified_packs};
use crate::store::{LocalStore, MessageContent, MessageMeta};

/// Pictures downloaded per read, one after the other: the UI asks again for the rest.
const PICTURES_PER_ROUND: usize = 32;

/// A change to a pack: one the UI asks for, or appending a sticker whose picture was uploaded.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PackChange {
  Add(StickerEntry),
  Other(StickerPackChange),
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Reads the packs, keeps what verifies against the verified roster and fetches pictures it
  /// lacks. A device that can't read them keeps the packs it has, and an older server leaves none.
  pub async fn refresh_sticker_packs(&mut self) -> Result<(), CoreError> {
    let listed = match self.api.list_sticker_packs().await {
      Ok(listed) => listed,
      Err(error) if error.is_not_found() => teamcraft_proto::ListStickerPacksResponse::default(),
      Err(error) => return Err(error.into()),
    };
    let workspace = self.workspace()?;
    let set = StickerSet {
      packs: verified_packs(&listed.packs, &workspace),
      limits: listed.limits,
    };
    store_set(&self.store, &set)?;
    Box::pin(self.fetch_sticker_pictures()).await;
    Ok(())
  }

  /// Admins: creates an empty pack named `name`. Returns its key.
  pub async fn create_sticker_pack(&mut self, name: String) -> Result<StickerPackKey, CoreError> {
    teamcraft_proto::check_sticker_pack_name(&name)
      .map_err(|error| CoreError::InvalidRequest(error.to_string()))?;
    let set = stored_set(&self.store)?;
    if set.packs.len() >= set.limits.max_packs as usize {
      return Err(CoreError::InvalidRequest(format!(
        "at most {} sticker packs",
        set.limits.max_packs
      )));
    }
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let pack = StickerPackKey::from_bytes(bytes);
    let document = StickerPackDocument {
      pack,
      version: 1,
      name,
      stickers: Vec::new(),
      signed_at_ms: self.now_ms(),
    };
    self.save_sticker_document(document).await?;
    Box::pin(self.refresh_sticker_packs()).await?;
    Ok(pack)
  }

  /// Admins: uploads `picture` and appends it to `pack` as `name` with `alt`. The checks of the
  /// server run here first, so a refusal names the reason before anything is sent.
  pub async fn add_sticker(
    &mut self,
    pack: StickerPackKey,
    name: String,
    alt: String,
    picture: Vec<u8>,
  ) -> Result<(), CoreError> {
    check_custom_emoji_shortcode(&name).map_err(|error| CoreError::InvalidRequest(error.to_string()))?;
    check_sticker_alt(&alt).map_err(|error| CoreError::InvalidRequest(error.to_string()))?;
    let set = stored_set(&self.store)?;
    let inspected = inspect_custom_emoji_picture(&picture, set.limits.max_bytes, set.limits.max_side_px)
      .map_err(|error| CoreError::InvalidRequest(error.to_string()))?;
    let sha256 = CustomEmojiHash::from_bytes(teamcraft_crypto::attachment::content_sha256(&picture));
    self
      .api
      .upload_sticker_picture(&UploadStickerPictureRequest {
        picture: Base64Bytes::new(picture.clone()),
      })
      .await?;
    // This device has the picture already; nothing to download.
    keep_picture(&self.store, &set, sha256, picture)?;
    let sticker = StickerEntry {
      name,
      alt,
      sha256,
      format: inspected.format,
    };
    self.change_pack(pack, PackChange::Add(sticker)).await
  }

  /// Admins: applies `change` to the newest version of `pack` this device verified and saves it as
  /// the next. If another admin saved a version meanwhile, it reads the packs again and applies the
  /// change once more on top of theirs.
  pub async fn change_sticker_pack(
    &mut self,
    pack: StickerPackKey,
    change: StickerPackChange,
  ) -> Result<(), CoreError> {
    self.change_pack(pack, PackChange::Other(change)).await
  }

  async fn change_pack(&mut self, pack: StickerPackKey, change: PackChange) -> Result<(), CoreError> {
    for is_retry in [false, true] {
      let document = self.next_sticker_document(pack, &change).await?;
      match self.save_sticker_document(document).await {
        Err(CoreError::Api(error))
          if !is_retry && error.error_code() == Some(&teamcraft_proto::ErrorCode::StickerPackStale) =>
        {
          Box::pin(self.refresh_sticker_packs()).await?;
        },
        other => {
          other?;
          break;
        },
      }
    }
    Box::pin(self.refresh_sticker_packs()).await
  }

  /// Admins: deletes a pack. Messages that sent its stickers keep showing them on devices that hold
  /// the pictures, and the fallback text elsewhere.
  pub async fn delete_sticker_pack(&mut self, pack: StickerPackKey) -> Result<(), CoreError> {
    match self.api.delete_sticker_pack(pack).await {
      Err(error) if error.is_not_found() => {},
      other => other?,
    }
    Box::pin(self.refresh_sticker_packs()).await
  }

  /// Sends a sticker (issue #2132, e2ee.md §4.14): a text message carrying `sticker`, whose text
  /// `fallback` is what versions before stickers and push previews show.
  pub async fn post_sticker(
    &mut self,
    conversation: teamcraft_proto::ConversationId,
    maybe_thread: Option<teamcraft_proto::ThreadId>,
    sticker: StickerRef,
    fallback: &str,
  ) -> Result<teamcraft_proto::ClientMessageId, CoreError> {
    let content = sticker_message(sticker, fallback);
    let id = self.enqueue(conversation, maybe_thread, content)?;
    self.flush_outbox().await?;
    Ok(id)
  }

  /// The document that follows the newest verified version of `pack` with `change` applied: its
  /// version above both that one and the server's newest, so a version this device couldn't
  /// verify can't keep the save stale forever.
  async fn next_sticker_document(
    &mut self,
    pack: StickerPackKey,
    change: &PackChange,
  ) -> Result<StickerPackDocument, CoreError> {
    let listed = self.api.list_sticker_packs().await?;
    let set = stored_set(&self.store)?;
    let current = set
      .pack(pack)
      .ok_or_else(|| CoreError::InvalidRequest("this sticker pack isn't in the verified set".into()))?;
    let listed_version = listed
      .packs
      .iter()
      .filter(|known| known.pack == pack)
      .map(|known| known.version)
      .max()
      .unwrap_or(0);
    let mut document = current.document();
    apply_change(&mut document, change)?;
    document.version = current.version.max(listed_version) + 1;
    document.signed_at_ms = self.now_ms();
    document
      .check(set.limits.max_stickers_per_pack)
      .map_err(|error| CoreError::InvalidRequest(error.to_string()))?;
    Ok(document)
  }

  async fn save_sticker_document(&mut self, document: StickerPackDocument) -> Result<(), CoreError> {
    let signed = sign_sticker_pack(&self.account.device, &self.account.certificate, &document)?;
    self
      .api
      .save_sticker_pack(document.pack, &SaveStickerPackRequest { signed })
      .await?;
    Ok(())
  }

  /// Downloads up to [`PICTURES_PER_ROUND`] pictures of the set this device lacks, checking each
  /// against its signed hash.
  async fn fetch_sticker_pictures(&mut self) {
    let set = match stored_set(&self.store) {
      Ok(set) => set,
      Err(error) => {
        tracing::warn!(%error, "could not read the sticker packs");
        return;
      },
    };
    let mut missing: Vec<CustomEmojiHash> = Vec::new();
    for sticker in set.packs.iter().flat_map(|pack| &pack.stickers) {
      if !missing.contains(&sticker.sha256) && !has_picture(&self.store, sticker.sha256).unwrap_or(true) {
        missing.push(sticker.sha256);
      }
    }
    let max_bytes = set.limits.max_bytes.min(MAX_STICKER_BYTES_LIMIT);
    for sha256 in missing.into_iter().take(PICTURES_PER_ROUND) {
      let downloaded = match self.api.download_sticker_picture(sha256).await {
        Ok(download) => {
          self
            .api
            .get_blob_capped(&download.download_url, download.size_bytes.min(max_bytes))
            .await
        },
        Err(error) => Err(error),
      };
      match downloaded.map(|bytes| keep_picture(&self.store, &set, sha256, bytes)) {
        Ok(Ok(true)) => {},
        Ok(Ok(false)) => tracing::warn!("a sticker picture doesn't match its signed hash"),
        Ok(Err(error)) => tracing::warn!(%error, "could not keep a sticker picture"),
        Err(error) => tracing::debug!(%error, "could not download a sticker picture"),
      }
    }
  }
}

/// A sticker as a message: the fallback as its text, the sticker inside the ciphertext.
pub(crate) fn sticker_message(sticker: StickerRef, fallback: &str) -> MessageContent {
  MessageContent::Text {
    body: fallback.to_owned(),
    mentions: Vec::new(),
    meta: MessageMeta {
      maybe_sticker: Some(Box::new(sticker)),
      ..MessageMeta::default()
    },
  }
}

fn apply_change(document: &mut StickerPackDocument, change: &PackChange) -> Result<(), CoreError> {
  let position = |name: &str| {
    document
      .stickers
      .iter()
      .position(|sticker| sticker.name == name)
      .ok_or_else(|| CoreError::InvalidRequest("no sticker of this name in the pack".into()))
  };
  let change = match change {
    PackChange::Add(sticker) => {
      document.stickers.push(sticker.clone());
      return Ok(());
    },
    PackChange::Other(change) => change,
  };
  match change {
    StickerPackChange::Rename { name } => document.name = name.clone(),
    StickerPackChange::Edit { name, new_name, alt } => {
      let at = position(name)?;
      document.stickers[at].name = new_name.clone();
      document.stickers[at].alt = alt.clone();
    },
    StickerPackChange::Remove { name } => {
      let at = position(name)?;
      document.stickers.remove(at);
    },
    StickerPackChange::Move { name, to } => {
      let at = position(name)?;
      let sticker = document.stickers.remove(at);
      let to = (*to).min(document.stickers.len());
      document.stickers.insert(to, sticker);
    },
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::CustomEmojiFormat;

  use super::*;

  fn entry(name: &str) -> StickerEntry {
    StickerEntry {
      name: name.into(),
      alt: format!("{name} alt"),
      sha256: CustomEmojiHash::from_bytes([name.len() as u8; 32]),
      format: CustomEmojiFormat::Png,
    }
  }

  fn names(document: &StickerPackDocument) -> Vec<&str> {
    document
      .stickers
      .iter()
      .map(|sticker| sticker.name.as_str())
      .collect()
  }

  #[test]
  fn changes_apply_to_the_document() {
    let mut document = StickerPackDocument {
      pack: StickerPackKey::from_bytes([1; 16]),
      version: 1,
      name: "Team".into(),
      stickers: vec![entry("a1"), entry("b22"), entry("c333")],
      signed_at_ms: 0,
    };
    apply_change(
      &mut document,
      &PackChange::Other(StickerPackChange::Move {
        name: "c333".into(),
        to: 0,
      }),
    )
    .unwrap();
    assert_eq!(names(&document), ["c333", "a1", "b22"]);
    apply_change(
      &mut document,
      &PackChange::Other(StickerPackChange::Move {
        name: "c333".into(),
        to: 99,
      }),
    )
    .unwrap();
    assert_eq!(names(&document), ["a1", "b22", "c333"]);
    apply_change(
      &mut document,
      &PackChange::Other(StickerPackChange::Remove { name: "b22".into() }),
    )
    .unwrap();
    assert_eq!(names(&document), ["a1", "c333"]);
    apply_change(
      &mut document,
      &PackChange::Other(StickerPackChange::Edit {
        name: "a1".into(),
        new_name: "wave".into(),
        alt: "Waving".into(),
      }),
    )
    .unwrap();
    assert_eq!(
      (
        document.stickers[0].name.as_str(),
        document.stickers[0].alt.as_str()
      ),
      ("wave", "Waving")
    );
    assert!(
      apply_change(
        &mut document,
        &PackChange::Other(StickerPackChange::Remove { name: "gone".into() })
      )
      .is_err()
    );
  }
}
