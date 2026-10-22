//! The user's own call backgrounds: each picture is encrypted with a fresh key and stored on the
//! server (`/me/backgrounds`); its key and name are account data inside the encrypted key bag, so
//! the user's other devices learn about it with the next key bag sync and nobody else can open it.

use rand_core::OsRng;
use teamcraft_crypto::attachment::{
  AttachmentKey, DEFAULT_CHUNK_SIZE, ciphertext_matches, decrypt_attachment, encrypt_attachment,
};
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::{BackgroundId, Base64Bytes, CreateBackgroundUploadRequest, ErrorCode};

use super::Client;
use crate::account_data::BackgroundEntry;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::store::LocalStore;

/// Smallest download cap for a background (covers the encryption overhead of tiny pictures).
const MIN_DOWNLOAD_CAP_BYTES: u64 = 64 * 1024;

/// One of the user's backgrounds, as a picker lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundSummary {
  pub id: BackgroundId,
  pub name: String,
  pub size_bytes: u64,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The user's backgrounds, oldest first.
  pub fn backgrounds(&self) -> Vec<BackgroundSummary> {
    self
      .account_data()
      .backgrounds()
      .into_iter()
      .map(|(id, entry)| BackgroundSummary {
        id,
        name: entry.name.clone(),
        size_bytes: entry.size_bytes,
      })
      .collect()
  }

  /// Encrypts and uploads a picture (the app has normalized it, e.g. to a JPEG without metadata)
  /// and adds it to the account data; the key bag carries it to the user's other devices.
  pub async fn add_background(&mut self, name: String, picture: Vec<u8>) -> Result<BackgroundId, CoreError> {
    let encrypted = encrypt_attachment(&picture, DEFAULT_CHUNK_SIZE, &mut OsRng)?;
    let created = self
      .api
      .create_background_upload(&CreateBackgroundUploadRequest {
        size_bytes: encrypted.ciphertext.len() as u64,
      })
      .await?;
    self
      .api
      .put_blob(&created.upload_url, encrypted.ciphertext)
      .await?;
    self.api.complete_background_upload(created.id).await?;
    let entry = BackgroundEntry {
      name,
      key: Base64Bytes::new(encrypted.key.as_key().expose_secret().to_vec()),
      ciphertext_sha256: Base64Bytes::new(encrypted.ciphertext_sha256.to_vec()),
      size_bytes: picture.len() as u64,
    };
    self.update_account_data(|data, now| {
      data.add_background(created.id, entry, now);
      true
    })?;
    Ok(created.id)
  }

  /// Downloads and decrypts a background (checking the blob's hash first).
  pub async fn background_picture(&self, id: BackgroundId) -> Result<SecretBytes, CoreError> {
    let data = self.account_data();
    let entry = data
      .background(id)
      .ok_or_else(|| CoreError::Corrupt("unknown background".into()))?;
    let download = self.api.download_background(id).await?;
    // Chunked encryption adds a little per chunk; the picture's own size bounds the download.
    let max_bytes = entry.size_bytes.saturating_mul(2).max(MIN_DOWNLOAD_CAP_BYTES);
    let ciphertext = self
      .api
      .get_blob_capped(&download.download_url, max_bytes)
      .await?;
    if !ciphertext_matches(&ciphertext, entry.ciphertext_sha256.as_slice()) {
      return Err(CoreError::Corrupt(
        "background blob doesn't match its hash".into(),
      ));
    }
    let key = SymmetricKey::from_slice(entry.key.as_slice())
      .ok_or_else(|| CoreError::Corrupt("bad background key".into()))?;
    Ok(decrypt_attachment(&AttachmentKey::from_key(key), &ciphertext)?)
  }

  /// Removes a background from the account data and deletes its object on the server.
  pub async fn delete_background(&mut self, id: BackgroundId) -> Result<(), CoreError> {
    self.update_account_data(|data, now| data.remove_background(id, now))?;
    match self.api.delete_background(id).await {
      // Another device deleted it first.
      Err(error) if error.error_code() == Some(&ErrorCode::BackgroundNotFound) => Ok(()),
      other => Ok(other?),
    }
  }
}
