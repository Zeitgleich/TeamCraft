//! Files beyond single attachments: sending text with several files (uploads resume per file
//! after a restart or network loss), downloading to disk, and share links (e2ee.md §4.5.1).

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::attachment::AttachmentKey;
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_crypto::share_link::create_fragment;
use teamcraft_proto::{
  AttachmentId, Base64Bytes, ClientMessageId, ConversationId, CreateShareLinkRequest, ErrorCode, UnixMillis,
};

use super::Client;
use super::attachments::OutgoingFile;
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::records;
use crate::store::{AttachmentRef, LocalStore, MessageContent, MessageFilter, Table};

/// Argon2id for share link passwords: the browser download page runs it in WebAssembly, so it is
/// lighter than the backup passphrase's (the link itself is the main secret).
const SHARE_LINK_ARGON2: Argon2Settings = Argon2Settings {
  memory_kib: 32 * 1024,
  iterations: 3,
  parallelism: 1,
};

/// Files of an outbox message that aren't uploaded yet; the message waits until they are.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingUpload {
  pub conversation_id: ConversationId,
  pub client_message_id: ClientMessageId,
  pub files: Vec<PendingFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingFile {
  pub file_name: String,
  pub media_type: String,
  pub source: UploadSource,
  pub size_bytes: u64,
  /// Set once uploaded.
  #[serde(rename = "done", default, skip_serializing_if = "Option::is_none")]
  pub maybe_done: Option<AttachmentRef>,
  #[serde(rename = "failure", default, skip_serializing_if = "Option::is_none")]
  pub maybe_failure: Option<TransferFailure>,
}

/// Where an upload reads from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UploadSource {
  /// Kept in the (encrypted) local store until uploaded.
  Bytes { bytes: Base64Bytes },
  /// A file on this device, read when uploading.
  Path { path: String },
}

/// Largest file the browser saves: it holds the decrypted bytes in memory until the `<a download>`
/// click, so bigger files wait for streaming downloads. Native builds write to disk and have no cap.
#[cfg(target_arch = "wasm32")]
pub(super) const MAX_DOWNLOAD_BYTES: Option<u64> = Some(256 * 1024 * 1024);
#[cfg(not(target_arch = "wasm32"))]
pub(super) const MAX_DOWNLOAD_BYTES: Option<u64> = None;

/// Longest saved file name in bytes; common file systems allow 255 and the copy suffix needs room.
const MAX_FILE_NAME_BYTES: usize = 200;

/// How long the browser gets to start reading a download before its object URL is revoked.
#[cfg(target_arch = "wasm32")]
const REVOKE_DELAY_MS: i32 = 1000;

/// Progress of a file on this device, keyed by attachment ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TransferState {
  Downloading { total_bytes: u64 },
  Local { path: String },
  Failed { reason: TransferFailure },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferFailure {
  TooLarge,
  Network,
  Corrupted,
  LocalFile,
}

/// A share link this user created (only its creator has the fragment).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredShareLink {
  pub attachment_id: AttachmentId,
  pub share_id: String,
  /// Including the `#fragment` with the key.
  pub url: String,
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<i64>,
  #[serde(rename = "max_downloads", default, skip_serializing_if = "Option::is_none")]
  pub maybe_max_downloads: Option<u32>,
  pub has_password: bool,
}

/// Options of a new share link.
#[derive(Default)]
pub struct ShareLinkSettings {
  pub maybe_expires_at: Option<i64>,
  pub maybe_max_downloads: Option<u32>,
  pub maybe_password: Option<SecretBytes>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Sends text with files: the message is stored at once and sent after its files are uploaded.
  pub async fn send_files(
    &mut self,
    conversation: ConversationId,
    maybe_thread: Option<teamcraft_proto::ThreadId>,
    body: &str,
    files: Vec<OutgoingFile>,
  ) -> Result<ClientMessageId, CoreError> {
    let content = MessageContent::text(body, Vec::new());
    let id = self.enqueue(conversation, maybe_thread, content)?;
    let pending = PendingUpload {
      conversation_id: conversation,
      client_message_id: id,
      files: files
        .into_iter()
        .map(|file| PendingFile {
          size_bytes: file.content.len() as u64,
          file_name: file.file_name,
          media_type: file.media_type,
          source: UploadSource::Bytes {
            bytes: Base64Bytes::new(file.content),
          },
          maybe_done: None,
          maybe_failure: None,
        })
        .collect(),
    };
    save_pending_upload(&self.store, &pending)?;
    self.upload_pending().await?;
    self.flush_outbox().await?;
    Ok(id)
  }

  /// Uploads what's pending, file by file; finished messages are completed and become sendable.
  /// Stops at the first network error (the rest resumes next time). Returns how many files went up.
  pub async fn upload_pending(&mut self) -> Result<usize, CoreError> {
    let mut uploaded = 0;
    let pending: Vec<PendingUpload> = records::list_json(&self.store, Table::PendingUploads)?;
    for mut upload in pending {
      for index in 0..upload.files.len() {
        let file = &upload.files[index];
        if file.maybe_done.is_some() || file.maybe_failure.is_some() {
          continue;
        }
        match self.upload_one(upload.conversation_id, file).await {
          Ok(attachment) => {
            upload.files[index].maybe_done = Some(attachment);
            uploaded += 1;
          },
          Err(UploadError::Retry(error)) => {
            save_pending_upload(&self.store, &upload)?;
            return Err(error);
          },
          Err(UploadError::Failed(reason)) => upload.files[index].maybe_failure = Some(reason),
        }
        save_pending_upload(&self.store, &upload)?;
      }
      self.complete_upload(&upload)?;
      self.remove_staged_copies(&upload);
    }
    Ok(uploaded)
  }

  /// Downloads and decrypts a file into the download directory; progress and the path are in
  /// [`transfer_state`].
  pub async fn download_attachment_to_disk(
    &mut self,
    conversation: ConversationId,
    attachment: AttachmentId,
  ) -> Result<String, CoreError> {
    let reference = find_attachment(&self.store, conversation, attachment)?
      .ok_or_else(|| CoreError::Corrupt("no such file in this conversation".into()))?;
    if exceeds_download_limit(reference.size_bytes, MAX_DOWNLOAD_BYTES) {
      set_transfer(
        &self.store,
        attachment,
        &TransferState::Failed {
          reason: TransferFailure::TooLarge,
        },
      )?;
      return Err(CoreError::InvalidRequest(
        "the file is too large to download here".into(),
      ));
    }
    set_transfer(
      &self.store,
      attachment,
      &TransferState::Downloading {
        total_bytes: reference.size_bytes,
      },
    )?;
    let result = self.download_to_disk(&reference).await;
    let state = match &result {
      Ok(path) => TransferState::Local { path: path.clone() },
      Err(error) => TransferState::Failed {
        reason: failure_of(error),
      },
    };
    set_transfer(&self.store, attachment, &state)?;
    result
  }

  /// Creates a public download link for a file of the conversation (e2ee.md §4.5.1).
  pub async fn create_share_link(
    &mut self,
    conversation: ConversationId,
    attachment: AttachmentId,
    settings: ShareLinkSettings,
  ) -> Result<StoredShareLink, CoreError> {
    let reference = find_attachment(&self.store, conversation, attachment)?
      .ok_or_else(|| CoreError::Corrupt("no such file in this conversation".into()))?;
    let key = SymmetricKey::from_slice(reference.key.as_slice())
      .ok_or_else(|| CoreError::Corrupt("bad attachment key".into()))?;
    let fragment = create_fragment(
      &AttachmentKey::from_key(key),
      &reference.file_name,
      settings.maybe_password.as_ref(),
      SHARE_LINK_ARGON2,
      &mut OsRng,
    )
    .map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let response = self
      .api
      .create_share_link(
        attachment,
        &CreateShareLinkRequest {
          attachment_id: attachment,
          maybe_expires_at: settings.maybe_expires_at.map(UnixMillis),
          maybe_max_downloads: settings.maybe_max_downloads,
          is_password_protected: settings.maybe_password.is_some(),
        },
      )
      .await?;
    let link = StoredShareLink {
      attachment_id: attachment,
      // The server only knows the link's URL when its public URL is configured.
      url: format!(
        "{}#{fragment}",
        response.maybe_url.clone().ok_or(ApiError::Unsupported(
          "the server has no public URL configured for share links"
        ))?
      ),
      share_id: response.id,
      maybe_expires_at: settings.maybe_expires_at,
      maybe_max_downloads: settings.maybe_max_downloads,
      has_password: settings.maybe_password.is_some(),
    };
    let mut links = share_links(&self.store, attachment)?;
    links.push(link.clone());
    records::put_json(&self.store, Table::ShareLinks, &attachment.to_string(), &links)?;
    Ok(link)
  }

  async fn upload_one(
    &self,
    conversation: ConversationId,
    file: &PendingFile,
  ) -> Result<AttachmentRef, UploadError> {
    if file.size_bytes > self.settings.max_upload_bytes {
      return Err(UploadError::Failed(TransferFailure::TooLarge));
    }
    let content = read_source(&file.source).map_err(|()| UploadError::Failed(TransferFailure::LocalFile))?;
    let maybe_thumbnail = crate::thumbnail::thumbnail_png(&content, &file.media_type);
    let outgoing = OutgoingFile {
      file_name: file.file_name.clone(),
      media_type: file.media_type.clone(),
      content,
    };
    match self.upload_encrypted(conversation, outgoing).await {
      Ok(mut attachment) => {
        attachment.maybe_thumbnail = maybe_thumbnail.map(Base64Bytes::new);
        Ok(attachment)
      },
      Err(error) if error.is_retry_later() => Err(UploadError::Retry(error)),
      Err(CoreError::Api(api))
        if api.error_code() == Some(&ErrorCode::PayloadTooLarge) || is_status(&api, 413) =>
      {
        Err(UploadError::Failed(TransferFailure::TooLarge))
      },
      Err(_) => Err(UploadError::Failed(TransferFailure::Network)),
    }
  }

  /// Deletes the app's staged plaintext copies of a finished upload (see
  /// [`super::ClientSettings::maybe_upload_staging_dir`]).
  fn remove_staged_copies(&self, upload: &PendingUpload) {
    let Some(staging) = &self.settings.maybe_upload_staging_dir else {
      return;
    };
    if !is_finished(upload) {
      return;
    }
    for file in &upload.files {
      remove_staged(staging, &file.source);
    }
  }

  /// Puts the uploaded files into the waiting message, which the outbox then sends.
  fn complete_upload(&mut self, upload: &PendingUpload) -> Result<(), CoreError> {
    if !is_finished(upload) {
      return Ok(());
    }
    let key = records::outbox_key(upload.conversation_id, upload.client_message_id);
    if let Some(mut stored) = self
      .store
      .message(upload.conversation_id, upload.client_message_id)?
      && let MessageContent::Text { meta, .. } = &mut stored.content
    {
      meta.attachments = upload
        .files
        .iter()
        .filter_map(|file| file.maybe_done.clone())
        .collect();
      self.store.upsert_message(&stored)?;
    }
    let failed: Vec<PendingFile> = upload
      .files
      .iter()
      .filter(|file| file.maybe_failure.is_some())
      .map(|file| PendingFile {
        // The bytes aren't needed any more, only the name and the reason.
        source: UploadSource::Bytes {
          bytes: Base64Bytes::new(Vec::new()),
        },
        ..file.clone()
      })
      .collect();
    if !failed.is_empty() {
      records::put_json(&self.store, Table::FileTransfers, &failed_key(&key), &failed)?;
    }
    self.store.delete(Table::PendingUploads, &key)?;
    Ok(())
  }

  #[cfg(not(target_arch = "wasm32"))]
  async fn download_to_disk(&self, reference: &AttachmentRef) -> Result<String, CoreError> {
    let directory = self
      .settings
      .maybe_download_dir
      .clone()
      .ok_or_else(|| CoreError::Corrupt("no download directory configured".into()))?;
    let plaintext = self.download_decrypted(reference).await?;
    std::fs::create_dir_all(&directory).map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let path = unique_path(&directory, &reference.file_name);
    std::fs::write(&path, plaintext.expose_secret())
      .map_err(|error| CoreError::Corrupt(error.to_string()))?;
    Ok(path.to_string_lossy().into_owned())
  }

  /// The browser has no download directory: the decrypted file is handed to the browser through a
  /// temporary `<a download>` link, which saves it into the browser's download folder.
  #[cfg(target_arch = "wasm32")]
  async fn download_to_disk(&self, reference: &AttachmentRef) -> Result<String, CoreError> {
    // The cap is enforced on the bytes actually received, not on the sender-declared size.
    let plaintext = self
      .download_decrypted_capped(reference, MAX_DOWNLOAD_BYTES)
      .await?;
    let file_name = sanitize_file_name(&reference.file_name);
    save_in_browser(plaintext.expose_secret(), &file_name)?;
    Ok(file_name)
  }
}

enum UploadError {
  /// Network trouble: keep the file pending and try again later.
  Retry(CoreError),
  Failed(TransferFailure),
}

/// Every file of a conversation's messages: single-file messages and files sent with text.
pub fn attachments_of(content: &MessageContent) -> Vec<&AttachmentRef> {
  match content {
    MessageContent::File { attachment } => vec![attachment],
    MessageContent::Text { meta, .. } => meta.attachments.iter().collect(),
    _ => Vec::new(),
  }
}

pub fn pending_upload(
  store: &impl LocalStore,
  conversation: ConversationId,
  message: ClientMessageId,
) -> Result<Option<PendingUpload>, CoreError> {
  records::get_json(
    store,
    Table::PendingUploads,
    &records::outbox_key(conversation, message),
  )
}

pub fn save_pending_upload(store: &impl LocalStore, upload: &PendingUpload) -> Result<(), CoreError> {
  records::put_json(
    store,
    Table::PendingUploads,
    &records::outbox_key(upload.conversation_id, upload.client_message_id),
    upload,
  )
}

/// Files of a sent message that couldn't be uploaded (shown to the sender only).
pub fn failed_uploads(
  store: &impl LocalStore,
  conversation: ConversationId,
  message: ClientMessageId,
) -> Result<Vec<PendingFile>, CoreError> {
  let key = failed_key(&records::outbox_key(conversation, message));
  Ok(records::get_json(store, Table::FileTransfers, &key)?.unwrap_or_default())
}

fn failed_key(outbox_key: &str) -> String {
  format!("failed/{outbox_key}")
}

pub fn transfer_state(
  store: &impl LocalStore,
  attachment: AttachmentId,
) -> Result<Option<TransferState>, CoreError> {
  records::get_json(store, Table::FileTransfers, &attachment.to_string())
}

pub fn share_links(
  store: &impl LocalStore,
  attachment: AttachmentId,
) -> Result<Vec<StoredShareLink>, CoreError> {
  Ok(records::get_json(store, Table::ShareLinks, &attachment.to_string())?.unwrap_or_default())
}

fn set_transfer(
  store: &impl LocalStore,
  attachment: AttachmentId,
  state: &TransferState,
) -> Result<(), CoreError> {
  records::put_json(store, Table::FileTransfers, &attachment.to_string(), state)
}

fn find_attachment(
  store: &impl LocalStore,
  conversation: ConversationId,
  attachment: AttachmentId,
) -> Result<Option<AttachmentRef>, CoreError> {
  Ok(
    store
      .messages(conversation, MessageFilter::All)?
      .iter()
      .flat_map(|message| {
        attachments_of(&message.content)
          .into_iter()
          .cloned()
          .collect::<Vec<_>>()
      })
      .find(|reference| reference.attachment_id == attachment),
  )
}

/// Every file of the upload went up or failed for good.
fn is_finished(upload: &PendingUpload) -> bool {
  upload
    .files
    .iter()
    .all(|file| file.maybe_done.is_some() || file.maybe_failure.is_some())
}

/// Deletes the staged plaintext copies in `staging` that no pending upload refers to: files taken
/// out of the composer before sending, or left by an app that was stopped first. Returns how many
/// were deleted. Call it before the app stages new files (when the client opens).
#[cfg(not(target_arch = "wasm32"))]
pub fn sweep_staged_copies(store: &impl LocalStore, staging: &std::path::Path) -> Result<usize, CoreError> {
  let uploads: Vec<PendingUpload> = records::list_json(store, Table::PendingUploads)?;
  let referenced: std::collections::HashSet<std::path::PathBuf> = uploads
    .iter()
    .flat_map(|upload| upload.files.iter())
    .filter_map(|file| match &file.source {
      UploadSource::Path { path } => Some(std::path::PathBuf::from(path)),
      UploadSource::Bytes { .. } => None,
    })
    .collect();
  let Ok(entries) = std::fs::read_dir(staging) else {
    return Ok(0);
  };
  let mut removed = 0;
  for entry in entries.flatten() {
    let path = entry.path();
    if entry.file_type().is_ok_and(|kind| kind.is_file()) && !referenced.contains(&path) {
      match std::fs::remove_file(&path) {
        Ok(()) => removed += 1,
        Err(error) => tracing::debug!(%error, "a stray staged upload copy could not be deleted"),
      }
    }
  }
  Ok(removed)
}

#[cfg(not(target_arch = "wasm32"))]
fn remove_staged(staging: &std::path::Path, source: &UploadSource) {
  let UploadSource::Path { path } = source else {
    return;
  };
  let path = std::path::Path::new(path);
  // Only the app's own copies; `..` can't lead out of the staging directory.
  let is_staged = path.starts_with(staging)
    && !path
      .components()
      .any(|part| part == std::path::Component::ParentDir);
  if is_staged && let Err(error) = std::fs::remove_file(path) {
    tracing::debug!(%error, "a staged upload copy could not be deleted");
  }
}

#[cfg(target_arch = "wasm32")]
fn remove_staged(_staging: &std::path::Path, _source: &UploadSource) {}

fn read_source(source: &UploadSource) -> Result<Vec<u8>, ()> {
  match source {
    UploadSource::Bytes { bytes } => Ok(bytes.as_slice().to_vec()),
    #[cfg(not(target_arch = "wasm32"))]
    UploadSource::Path { path } => std::fs::read(path).map_err(|error| {
      tracing::warn!(%error, "could not read a file to upload");
    }),
    #[cfg(target_arch = "wasm32")]
    UploadSource::Path { .. } => Err(()),
  }
}

fn failure_of(error: &CoreError) -> TransferFailure {
  match error {
    CoreError::Api(ApiError::BlobTooLarge { .. }) => TransferFailure::TooLarge,
    CoreError::Api(_) => TransferFailure::Network,
    CoreError::Attachment(_) => TransferFailure::Corrupted,
    CoreError::Corrupt(message) if message.contains("hash") => TransferFailure::Corrupted,
    _ => TransferFailure::LocalFile,
  }
}

fn is_status(error: &ApiError, status: u16) -> bool {
  matches!(error, ApiError::Status { status: actual, .. } if *actual == status)
}

/// Makes a sender-chosen file name safe to save: path separators and control characters become `_`
/// and leading dots are dropped, so a file can't land outside the download folder or hide.
fn sanitize_file_name(file_name: &str) -> String {
  let safe: String = file_name
    .chars()
    .map(|character| {
      if matches!(character, '/' | '\\') || character.is_control() {
        '_'
      } else {
        character
      }
    })
    .collect();
  let trimmed = safe.trim_start_matches('.').trim();
  let limited = truncate_utf8(trimmed, MAX_FILE_NAME_BYTES);
  if limited.is_empty() {
    return "file".to_string();
  }
  if is_reserved_windows_name(limited) {
    return format!("_{limited}");
  }
  limited.to_string()
}

/// Longest part of `text` that fits `max_bytes` without splitting a character.
fn truncate_utf8(text: &str, max_bytes: usize) -> &str {
  let mut end = max_bytes.min(text.len());
  while !text.is_char_boundary(end) {
    end -= 1;
  }
  &text[..end]
}

/// `CON`, `NUL`, `COM1`, ... (with or without an extension) can't be created on Windows.
fn is_reserved_windows_name(file_name: &str) -> bool {
  let stem = file_name
    .split('.')
    .next()
    .unwrap_or_default()
    .trim_end()
    .to_ascii_uppercase();
  matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
    || (stem.len() == 4
      && (stem.starts_with("COM") || stem.starts_with("LPT"))
      && stem.ends_with(|digit: char| ('1'..='9').contains(&digit)))
}

/// Whether a file of `size_bytes` is over the platform's download cap (`None`: no cap).
fn exceeds_download_limit(size_bytes: u64, maybe_max_bytes: Option<u64>) -> bool {
  maybe_max_bytes.is_some_and(|max| size_bytes > max)
}

/// Offers `bytes` to the browser as a download: Blob, object URL, a clicked `<a download>`, and the
/// URL is revoked right after.
#[cfg(target_arch = "wasm32")]
fn save_in_browser(bytes: &[u8], file_name: &str) -> Result<(), CoreError> {
  use wasm_bindgen::JsCast;

  let browser = |what: &str| CoreError::Browser(what.to_string());
  let document = web_sys::window()
    .and_then(|window| window.document())
    .ok_or_else(|| browser("no document"))?;
  let options = web_sys::BlobPropertyBag::new();
  options.set_type("application/octet-stream");
  let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
  let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options)
    .map_err(|_| browser("could not build the file"))?;
  let url =
    web_sys::Url::create_object_url_with_blob(&blob).map_err(|_| browser("could not build the file"))?;
  let anchor = document
    .create_element("a")
    .ok()
    .and_then(|element| element.dyn_into::<web_sys::HtmlAnchorElement>().ok());
  let result = match anchor {
    Some(anchor) => {
      anchor.set_href(&url);
      anchor.set_download(file_name);
      anchor.set_hidden(true);
      // Some browsers only honour clicks on attached elements.
      let attached = document
        .body()
        .is_some_and(|body| body.append_child(&anchor).is_ok());
      anchor.click();
      if attached {
        anchor.remove();
      }
      Ok(())
    },
    None => Err(browser("could not create a download link")),
  };
  // Revoking right after click() makes Firefox and Safari drop the download, so give them time.
  // Memory peaks at about three copies of the file (ciphertext, plaintext, Blob) until then.
  revoke_later(url);
  result
}

/// Revokes an object URL after [`REVOKE_DELAY_MS`].
#[cfg(target_arch = "wasm32")]
fn revoke_later(url: String) {
  use wasm_bindgen::JsCast;
  use wasm_bindgen::closure::Closure;

  let revoke = Closure::once_into_js(move || {
    if let Err(error) = web_sys::Url::revoke_object_url(&url) {
      tracing::debug!(?error, "the download URL could not be revoked");
    }
  });
  let scheduled = web_sys::window().and_then(|window| {
    window
      .set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), REVOKE_DELAY_MS)
      .ok()
  });
  if scheduled.is_none() {
    tracing::debug!("the download URL could not be scheduled for revoking");
  }
}

/// `name`, or `name (2)`, `name (3)`, ... if taken. The name is sanitized first.
#[cfg(not(target_arch = "wasm32"))]
fn unique_path(directory: &std::path::Path, file_name: &str) -> std::path::PathBuf {
  let safe = sanitize_file_name(file_name);
  let candidate = directory.join(&safe);
  if !candidate.exists() {
    return candidate;
  }
  let (stem, extension) = match safe.rsplit_once('.') {
    Some((stem, extension)) => (stem.to_string(), format!(".{extension}")),
    None => (safe.clone(), String::new()),
  };
  (2..)
    .map(|number| directory.join(format!("{stem} ({number}){extension}")))
    .find(|path| !path.exists())
    .unwrap_or(candidate)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
  use super::*;

  #[test]
  fn sanitized_names_have_no_separators_or_control_characters() {
    assert_eq!(sanitize_file_name("../../etc/passwd"), "_.._etc_passwd");
    assert_eq!(sanitize_file_name("a\\b/c.txt"), "a_b_c.txt");
    assert_eq!(sanitize_file_name("tab\there\u{7}\0.txt"), "tab_here__.txt");
    assert_eq!(sanitize_file_name("...hidden"), "hidden");
    assert_eq!(sanitize_file_name(".."), "file");
    assert_eq!(sanitize_file_name(""), "file");
    assert_eq!(sanitize_file_name("CON"), "_CON");
    assert_eq!(sanitize_file_name("nul.txt"), "_nul.txt");
    assert_eq!(sanitize_file_name("com1.tar.gz"), "_com1.tar.gz");
    assert_eq!(sanitize_file_name("LPT9"), "_LPT9");
    assert_eq!(sanitize_file_name("console.txt"), "console.txt");
    assert_eq!(sanitize_file_name("COM0"), "COM0");
    assert_eq!(sanitize_file_name(&"a".repeat(500)).len(), MAX_FILE_NAME_BYTES);
    // 200 is not a boundary of the 3-byte characters: the cut stays valid UTF-8.
    assert_eq!(sanitize_file_name(&"€".repeat(100)).len(), 198);
    assert_eq!(sanitize_file_name("report (final).pdf"), "report (final).pdf");
  }

  #[test]
  fn oversized_blobs_are_a_too_large_failure_not_a_network_one() {
    let error = CoreError::Api(ApiError::BlobTooLarge { max_bytes: 10 });
    assert_eq!(failure_of(&error), TransferFailure::TooLarge);
  }

  #[test]
  fn only_capped_platforms_refuse_large_downloads() {
    assert!(!exceeds_download_limit(u64::MAX, None));
    assert!(!exceeds_download_limit(100, Some(100)));
    assert!(exceeds_download_limit(101, Some(100)));
    // Native builds write to disk and have no cap; the browser does.
    assert_eq!(MAX_DOWNLOAD_BYTES.is_some(), cfg!(target_arch = "wasm32"));
  }

  #[test]
  fn downloaded_names_stay_in_the_directory_and_do_not_overwrite() {
    let directory = std::env::temp_dir().join(format!(
      "teamcraft-files-{}",
      rand_core::RngCore::next_u64(&mut OsRng)
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let escaped = unique_path(&directory, "../../etc/passwd");
    assert_eq!(escaped.parent(), Some(directory.as_path()));
    assert_eq!(unique_path(&directory, "...").file_name().unwrap(), "file");
    std::fs::write(directory.join("a.txt"), b"x").unwrap();
    assert_eq!(unique_path(&directory, "a.txt").file_name().unwrap(), "a (2).txt");
    std::fs::remove_dir_all(&directory).unwrap();
  }

  #[test]
  fn only_staged_copies_are_deleted() {
    let root = std::env::temp_dir().join(format!(
      "teamcraft-staging-{}",
      rand_core::RngCore::next_u64(&mut OsRng)
    ));
    let staging = root.join("outgoing");
    std::fs::create_dir_all(&staging).unwrap();
    let staged = staging.join("copy.pdf");
    let original = root.join("original.pdf");
    std::fs::write(&staged, b"x").unwrap();
    std::fs::write(&original, b"x").unwrap();
    let escaping = format!("{}/../original.pdf", staging.display());

    for path in [
      &staged.display().to_string(),
      &original.display().to_string(),
      &escaping,
    ] {
      remove_staged(&staging, &UploadSource::Path { path: path.clone() });
    }
    assert!(!staged.exists());
    assert!(original.exists());
    std::fs::remove_dir_all(&root).unwrap();
  }

  #[test]
  fn the_sweep_keeps_copies_of_pending_uploads_only() {
    let staging = std::env::temp_dir().join(format!(
      "teamcraft-sweep-{}",
      rand_core::RngCore::next_u64(&mut OsRng)
    ));
    std::fs::create_dir_all(&staging).unwrap();
    let pending = staging.join("pending.pdf");
    let stray = staging.join("taken-out.pdf");
    std::fs::write(&pending, b"x").unwrap();
    std::fs::write(&stray, b"x").unwrap();
    let store = crate::store::MemoryStore::new();
    let upload = PendingUpload {
      conversation_id: teamcraft_proto::Id::from_snowflake(teamcraft_proto::Snowflake::new(1).unwrap()),
      client_message_id: ClientMessageId::from_bytes([1; 16]),
      files: vec![PendingFile {
        file_name: "pending.pdf".into(),
        media_type: "application/pdf".into(),
        source: UploadSource::Path {
          path: pending.display().to_string(),
        },
        size_bytes: 1,
        maybe_done: None,
        maybe_failure: None,
      }],
    };
    save_pending_upload(&store, &upload).unwrap();

    assert_eq!(sweep_staged_copies(&store, &staging).unwrap(), 1);
    assert!(pending.exists(), "still to be uploaded");
    assert!(!stray.exists());
    assert_eq!(sweep_staged_copies(&store, &staging.join("missing")).unwrap(), 0);
    std::fs::remove_dir_all(&staging).unwrap();
  }
}
