//! Attachments (e2ee.md §4.5): each file is encrypted with a fresh key in chunks, uploaded to object
//! storage through a presigned URL, and the key travels inside the encrypted message.

use rand_core::OsRng;
use teamcraft_crypto::attachment::{
  AttachmentKey, DEFAULT_CHUNK_SIZE, ciphertext_matches, decrypt_attachment, encrypt_attachment,
};
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::{
  AttachmentId, Base64Bytes, ClientMessageId, ConversationId, CreateAttachmentRequest, ThreadId,
};

use super::Client;
use crate::api::{DetachedDownload, ServerApi};
use crate::error::CoreError;
use crate::store::{AttachmentRef, LocalStore, MessageContent};
use crate::transfers::TransferGuard;

/// A file to send.
#[derive(Clone)]
pub struct OutgoingFile {
  pub file_name: String,
  pub media_type: String,
  pub content: Vec<u8>,
}

/// A file on its way down, detached from the client: a host drives it beside its loop, so a large
/// file doesn't hold up everything else (issue #491). Dropping it stops the transfer.
pub struct BlobDownload {
  reference: AttachmentRef,
  request: DetachedDownload,
  /// Lists the download's progress while it lives.
  _progress: TransferGuard,
}

/// Room above the plaintext cap for nonce, tag and framing of an encrypted attachment.
const CIPHERTEXT_OVERHEAD_BYTES: u64 = 1024;

/// Largest file the in-memory viewer (the image and video lightbox) opens, on every platform: the
/// decrypted bytes are held in memory, and the webview copies them once more into a `Blob`. Saving
/// to disk is not limited by this. The UI offers "Save" instead for bigger files.
pub const MAX_VIEWER_BYTES: u64 = 128 * 1024 * 1024;

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Encrypts and uploads a file, then posts the message that carries its key.
  pub async fn send_file(
    &mut self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    file: OutgoingFile,
  ) -> Result<ClientMessageId, CoreError> {
    let attachment = self.upload_encrypted(conversation, file).await?;
    let id = self.enqueue(conversation, maybe_thread, MessageContent::File { attachment })?;
    self.flush_outbox().await?;
    Ok(id)
  }

  /// Downloads and decrypts the file of a message, checking the blob's hash first.
  pub async fn fetch_file(
    &mut self,
    conversation: ConversationId,
    message: ClientMessageId,
  ) -> Result<SecretBytes, CoreError> {
    let stored = self
      .store
      .message(conversation, message)?
      .ok_or_else(|| CoreError::Corrupt("unknown message".into()))?;
    let MessageContent::File { attachment } = stored.content else {
      return Err(CoreError::Corrupt("the message has no file".into()));
    };
    self.download_decrypted(&attachment).await
  }

  /// Downloads and decrypts one file for showing it (the image viewer); nothing is written to disk.
  /// The reference comes from [`crate::ui_backend::CoreChatBackend::attachment_ref`].
  pub async fn open_attachment(&self, attachment: &AttachmentRef) -> Result<SecretBytes, CoreError> {
    self.start_viewer_download(attachment).await?.open().await
  }

  /// [`Self::open_attachment`] for a host that runs the transfer beside its loop: asks for the
  /// download and hands it back to drive ([`BlobDownload::open`]).
  pub async fn start_viewer_download(&self, attachment: &AttachmentRef) -> Result<BlobDownload, CoreError> {
    // A file that says it is too big is not fetched at all.
    if attachment.size_bytes > MAX_VIEWER_BYTES {
      return Err(CoreError::InvalidRequest("the file is too large to show".into()));
    }
    // The sender-declared size can't be trusted: the cap applies to the bytes actually received.
    self.start_download(attachment, Some(MAX_VIEWER_BYTES)).await
  }

  /// Encrypts with a fresh key, uploads through a presigned URL and returns what the message needs.
  pub(crate) async fn upload_encrypted(
    &self,
    conversation: ConversationId,
    file: OutgoingFile,
  ) -> Result<AttachmentRef, CoreError> {
    let encrypted = encrypt_attachment(&file.content, DEFAULT_CHUNK_SIZE, &mut OsRng)?;
    let created = self
      .api
      .create_attachment(&CreateAttachmentRequest {
        conversation_id: conversation,
        size_bytes: encrypted.ciphertext.len() as u64,
      })
      .await?;
    self
      .api
      .put_blob(&created.upload_url, encrypted.ciphertext)
      .await?;
    self.api.complete_attachment(created.id).await?;
    Ok(AttachmentRef {
      attachment_id: created.id,
      key: Base64Bytes::new(encrypted.key.as_key().expose_secret().to_vec()),
      ciphertext_sha256: Base64Bytes::new(encrypted.ciphertext_sha256.to_vec()),
      file_name: file.file_name,
      media_type: file.media_type,
      size_bytes: file.content.len() as u64,
      maybe_thumbnail: None,
      maybe_dimensions: None,
      maybe_duration_ms: None,
    })
  }

  pub(crate) async fn download_decrypted(
    &self,
    attachment: &AttachmentRef,
  ) -> Result<SecretBytes, CoreError> {
    self.download_decrypted_capped(attachment, None).await
  }

  /// Like [`Self::download_decrypted`], but stops reading once the ciphertext exceeds
  /// `maybe_max_plaintext_bytes` (plus encryption overhead): the sender-declared size can't be trusted.
  pub(crate) async fn download_decrypted_capped(
    &self,
    attachment: &AttachmentRef,
    maybe_max_plaintext_bytes: Option<u64>,
  ) -> Result<SecretBytes, CoreError> {
    self
      .start_download(attachment, maybe_max_plaintext_bytes)
      .await?
      .open()
      .await
  }

  /// Asks the server for the file's download URL and starts the transfer, listed with its progress
  /// while it runs.
  pub(crate) async fn start_download(
    &self,
    attachment: &AttachmentRef,
    maybe_max_plaintext_bytes: Option<u64>,
  ) -> Result<BlobDownload, CoreError> {
    let download = self.api.download_attachment(attachment.attachment_id).await?;
    let progress = self
      .transfers
      .start(attachment.attachment_id, attachment.size_bytes);
    let maybe_max_bytes = maybe_max_plaintext_bytes.map(|max| max.saturating_add(CIPHERTEXT_OVERHEAD_BYTES));
    let request = self
      .api
      .get_blob_detached(&download.download_url, maybe_max_bytes, progress.counter());
    Ok(BlobDownload {
      reference: attachment.clone(),
      request,
      _progress: progress,
    })
  }
}

impl BlobDownload {
  pub fn attachment_id(&self) -> AttachmentId {
    self.reference.attachment_id
  }

  /// Receives the file, checks it against the hash its message carries and decrypts it.
  pub async fn open(self) -> Result<SecretBytes, CoreError> {
    let Self {
      reference, request, ..
    } = self;
    let ciphertext = request.await?;
    if !ciphertext_matches(&ciphertext, reference.ciphertext_sha256.as_slice()) {
      return Err(CoreError::Corrupt(
        "attachment blob doesn't match its hash".into(),
      ));
    }
    let key = SymmetricKey::from_slice(reference.key.as_slice())
      .ok_or_else(|| CoreError::Corrupt("bad attachment key".into()))?;
    Ok(decrypt_attachment(&AttachmentKey::from_key(key), &ciphertext)?)
  }

  pub(crate) fn reference(&self) -> &AttachmentRef {
    &self.reference
  }
}
