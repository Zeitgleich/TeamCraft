//! Files beyond single attachments: sending text with several files (uploads resume per file
//! after a restart or network loss), downloading to disk, and share links (e2ee.md §4.5.1).

use std::pin::Pin;

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::attachment::{AttachmentKey, DEFAULT_CHUNK_SIZE, encrypt_attachment};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_crypto::share_link::create_fragment;
use teamcraft_proto::{
  AttachmentId, Base64Bytes, ClientMessageId, ConversationId, CreateAttachmentRequest,
  CreateShareLinkRequest, ErrorCode,
};

use super::Client;
use super::attachments::{BlobDownload, OutgoingFile};
use super::upload_parts::{PartOutcome, PartTarget, PartsUpload, is_multipart};
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::records;
use crate::store::{AttachmentRef, ImageDimensions, LocalStore, MessageContent, MessageFilter, Table};
use crate::transfers::TransferGuard;

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
  /// The server's upload slot once announced. A retry uploads into the same slot instead of
  /// announcing a new attachment: every announcement counts against the user's unfinished uploads
  /// until it completes or expires, so retrying with new ones ends in 409 Conflict.
  #[serde(rename = "slot", default, skip_serializing_if = "Option::is_none")]
  pub maybe_slot: Option<UploadSlot>,
  /// Failed attempts and when the next one is due.
  #[serde(default, skip_serializing_if = "UploadRetry::is_fresh")]
  pub retry: UploadRetry,
  /// The app's own thumbnail, used when the core can't make one (see `OutgoingFile`). Without one,
  /// the core's is kept here once the upload started. The pending message shows it.
  #[serde(rename = "thumbnail", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thumbnail: Option<Base64Bytes>,
  /// The picture's upright size, so the pending message reserves the space the sent one takes.
  #[serde(rename = "dimensions", default, skip_serializing_if = "Option::is_none")]
  pub maybe_dimensions: Option<ImageDimensions>,
  /// The video's length, from the sender's app; it goes into the attachment's details.
  #[serde(rename = "duration_ms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_duration_ms: Option<u64>,
  /// A large file's upload in parts once announced: a restart continues at its next part.
  #[serde(rename = "parts", default, skip_serializing_if = "Option::is_none")]
  pub maybe_parts: Option<PartsUpload>,
}

/// One file's encrypted bytes on their way to storage. The client prepares it
/// ([`Client::start_upload`]), the host drives the request beside its loop ([`Self::sent`]) so the
/// app stays responsive while a large file goes up, and the client completes it
/// ([`Client::finish_upload`]).
pub struct BlobUpload {
  conversation_id: ConversationId,
  client_message_id: ClientMessageId,
  target: UploadTarget,
  /// `None` when nothing goes to the storage (the parts of a file are only joined).
  maybe_request: Option<BlobRequest>,
  /// What storage answered once [`Self::sent`] resolved successfully: a part's `ETag`.
  maybe_etag: Option<String>,
  /// Lists the upload's progress until it is finished or dropped (kept across a file's parts).
  progress: TransferGuard,
}

/// What a [`BlobUpload`] sends.
enum UploadTarget {
  /// A file in one blob, into its announced slot.
  Whole {
    slot: UploadSlot,
    attachment: AttachmentRef,
  },
  /// One part of a large file (or only the joining of its parts).
  Part(PartTarget),
}

/// Largest thumbnail an app may hand in: as large as the core's own (the message carries it); a
/// bigger one is dropped and the file goes without.
pub const MAX_APP_THUMBNAIL_BYTES: usize = crate::thumbnail::MAX_THUMBNAIL_BYTES;

/// An announced attachment that the file's ciphertext goes into.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadSlot {
  pub attachment_id: AttachmentId,
  pub upload_url: String,
  /// The announced ciphertext size; the presigned URL accepts exactly this many bytes.
  pub size_bytes: u64,
}

/// Failed attempts of one file since it was queued or the user retried it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UploadRetry {
  /// Not failed yet: due right away.
  #[default]
  Fresh,
  /// Failed `attempts` times; not tried again before `retry_at_ms` (client clock: it only spaces
  /// out retries).
  Waiting {
    attempts: u32,
    retry_at_ms: i64,
    #[serde(default, skip_serializing_if = "RetryCause::is_network")]
    cause: RetryCause,
  },
  /// Gave up retrying by itself; waits for the user's "Retry" ([`retry_stalled_upload`]).
  Stalled {
    attempts: u32,
    #[serde(default, skip_serializing_if = "RetryCause::is_network")]
    cause: RetryCause,
  },
}

/// Why the last attempt of a file failed, so a stalled file can say so (#742).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryCause {
  /// The transfer or the server failed (also every record stored before causes were kept).
  #[default]
  Network,
  /// The file storage answered "unavailable" (503), directly or through the server: an outage of
  /// the storage provider that a later retry gets through.
  StorageUnavailable,
}

/// How uploads that failed on the network are retried. Configurable; the defaults give up after
/// about a minute of failures (2, 4, 8, 16 and 32 seconds apart) and leave the file to the user's
/// "Retry", which says why it stopped (#742).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadRetrySettings {
  /// Wait after the first failed attempt; doubles with each further one.
  pub retry_after: std::time::Duration,
  /// The wait between attempts grows up to this.
  pub retry_max: std::time::Duration,
  /// Failed attempts after which the file waits for the user (shown as failed, with "Retry").
  pub max_attempts: u32,
}

/// Where an upload reads from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UploadSource {
  /// Kept in the (encrypted) local store until uploaded.
  Bytes { bytes: Base64Bytes },
  /// A file on this device, read when uploading.
  Path { path: String },
  /// A file the user picked in the browser, read in slices while it goes up
  /// ([`crate::picked_files`]).
  Picked { handle: String },
}

impl Default for UploadRetrySettings {
  fn default() -> Self {
    Self {
      retry_after: std::time::Duration::from_secs(2),
      retry_max: std::time::Duration::from_secs(60),
      max_attempts: 6,
    }
  }
}

impl UploadRetry {
  pub fn is_stalled(&self) -> bool {
    matches!(self, Self::Stalled { .. })
  }

  pub fn failed_attempts(&self) -> u32 {
    match self {
      Self::Fresh => 0,
      Self::Waiting { attempts, .. } | Self::Stalled { attempts, .. } => *attempts,
    }
  }

  /// Why it stalled, once it did.
  pub fn stall_cause(&self) -> Option<RetryCause> {
    match self {
      Self::Stalled { cause, .. } => Some(*cause),
      Self::Fresh | Self::Waiting { .. } => None,
    }
  }

  /// Whether an attempt may run at `now_ms`.
  pub fn is_due(&self, now_ms: i64) -> bool {
    match self {
      Self::Fresh => true,
      Self::Waiting { retry_at_ms, .. } => now_ms >= *retry_at_ms,
      Self::Stalled { .. } => false,
    }
  }

  /// Counts a failed attempt: the next one waits with exponential backoff, and after
  /// [`UploadRetrySettings::max_attempts`] the file stalls, keeping `cause` for the user.
  pub fn record_failure(&mut self, now_ms: i64, settings: &UploadRetrySettings, cause: RetryCause) {
    self.record_failure_after(now_ms, settings, cause, None);
  }

  /// [`Self::record_failure`] for an answer that said when to come back (`Retry-After`, #749): the
  /// next attempt waits at least that long, but never longer than
  /// [`UploadRetrySettings::retry_max`], so a hostile or misconfigured server can't park a file.
  pub fn record_failure_after(
    &mut self,
    now_ms: i64,
    settings: &UploadRetrySettings,
    cause: RetryCause,
    maybe_server_wait: Option<std::time::Duration>,
  ) {
    let attempts = match self {
      Self::Fresh => 1,
      Self::Waiting { attempts, .. } | Self::Stalled { attempts, .. } => attempts.saturating_add(1),
    };
    if attempts >= settings.max_attempts {
      *self = Self::Stalled { attempts, cause };
      return;
    }
    let base = millis(settings.retry_after);
    let ceiling = millis(settings.retry_max).max(base);
    let doublings = attempts.saturating_sub(1).min(32);
    let backoff = base.saturating_mul(1_i64 << doublings).min(ceiling);
    let server_wait = maybe_server_wait.map_or(0, |wait| millis(wait).min(ceiling));
    let wait = backoff.max(server_wait);
    *self = Self::Waiting {
      attempts,
      retry_at_ms: now_ms.saturating_add(wait),
      cause,
    };
  }

  fn is_fresh(&self) -> bool {
    *self == Self::Fresh
  }
}

impl RetryCause {
  /// What `error` says about the attempt: a 503 (or the server's `unavailable` code) is the storage
  /// or the server being down for a while; anything else counts as a network failure.
  pub fn of(error: &CoreError) -> Self {
    match error {
      CoreError::Api(api) if is_status(api, 503) || api.error_code() == Some(&ErrorCode::Unavailable) => {
        Self::StorageUnavailable
      },
      _ => Self::Network,
    }
  }

  fn is_network(&self) -> bool {
    *self == Self::Network
  }
}

fn millis(duration: std::time::Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// Largest file the browser saves: it holds the decrypted bytes in memory until the `<a download>`
/// click, so bigger files wait for streaming downloads. Native builds write to disk and have no cap.
#[cfg(target_arch = "wasm32")]
pub(super) const MAX_DOWNLOAD_BYTES: Option<u64> = Some(256 * 1024 * 1024);
#[cfg(not(target_arch = "wasm32"))]
pub(super) const MAX_DOWNLOAD_BYTES: Option<u64> = None;

/// Unfinished uploads the user cancelled, to give up at the server (it frees their parts and they
/// stop counting against the unfinished uploads); kept until the server is reached.
const ABANDONED_KEY: &str = "abandoned";

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
  /// Downloads so far, as of the last refresh from the server.
  #[serde(default)]
  pub download_count: u32,
}

/// Options of a new share link.
#[derive(Default)]
pub struct ShareLinkSettings {
  /// How long the link lasts from now; the server counts from its own clock and caps it.
  pub maybe_expires_in_seconds: Option<u64>,
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
          maybe_slot: None,
          maybe_parts: None,
          retry: UploadRetry::default(),
          maybe_thumbnail: None,
          maybe_dimensions: None,
          maybe_duration_ms: None,
        })
        .collect(),
    };
    save_pending_upload(&self.store, &pending)?;
    self.upload_pending().await?;
    self.flush_outbox().await?;
    Ok(id)
  }

  /// Uploads what's pending, file by file; finished messages are completed and become sendable.
  /// Stops at the first network error (the rest resumes next time). A file that failed waits with
  /// exponential backoff, and after [`UploadRetrySettings::max_attempts`] for the user's retry
  /// ([`retry_stalled_upload`]). Returns how many files went up.
  ///
  /// Holds the client for the whole upload; a host loop that must stay responsive drives
  /// [`Self::start_upload`] and [`Self::finish_upload`] itself.
  pub async fn upload_pending(&mut self) -> Result<usize, CoreError> {
    let mut uploaded = 0;
    while let Some(mut upload) = self.start_upload().await? {
      let sent = upload.sent().await;
      if self.finish_upload(upload, sent).await? == UploadStep::Uploaded {
        uploaded += 1;
      }
    }
    Ok(uploaded)
  }

  /// Whether the file `upload` sends still waits for it: not cancelled, its message not deleted or
  /// retried meanwhile. A host drops an upload that isn't, which stops it.
  pub fn is_upload_current(&self, upload: &BlobUpload) -> bool {
    self.current_file(upload).ok().flatten().is_some()
  }

  /// The pending upload `upload` belongs to and the index of its file, found by the upload slot or
  /// the part it sends (a cancelled file before it moves the index).
  fn current_file(&self, upload: &BlobUpload) -> Result<Option<(PendingUpload, usize)>, CoreError> {
    let Some(pending) = pending_upload(&self.store, upload.conversation_id, upload.client_message_id)? else {
      return Ok(None);
    };
    let maybe_index = pending.files.iter().position(|file| {
      file.maybe_done.is_none() && file.maybe_failure.is_none() && upload.target.is_sending(file)
    });
    Ok(maybe_index.map(|index| (pending, index)))
  }

  /// Prepares the next file that is due: encrypts it, keeps its thumbnail and size for the pending
  /// message, and announces it if it has no upload slot yet. `Ok(None)`: nothing is due. A file
  /// that can never go up is marked failed on the way; a network error stops the search (the rest
  /// resumes next time).
  pub async fn start_upload(&mut self) -> Result<Option<BlobUpload>, CoreError> {
    self.abandon_cancelled_uploads().await;
    let pending: Vec<PendingUpload> = records::list_json(&self.store, Table::PendingUploads)?;
    for mut upload in pending {
      while let Some(index) = next_due_file(&upload, self.now_ms()) {
        match self
          .start_file(upload.conversation_id, &mut upload.files[index])
          .await
        {
          Ok((target, maybe_request, progress)) => {
            save_pending_upload(&self.store, &upload)?;
            return Ok(Some(BlobUpload {
              conversation_id: upload.conversation_id,
              client_message_id: upload.client_message_id,
              target,
              maybe_request,
              maybe_etag: None,
              progress,
            }));
          },
          Err(error) => self.record_upload_error(&mut upload, index, error)?,
        }
      }
      self.complete_upload(&upload)?;
      self.remove_staged_copies(&upload);
    }
    // Nothing goes up: a progress entry kept from a part is not shown any longer.
    self.held_upload_progress = None;
    self.sweep_picked_files().await;
    Ok(None)
  }

  /// Completes a blob upload the host drove: confirms it with the server and puts the file into its
  /// message, which the outbox sends once all its files are up. A message deleted or retried
  /// meanwhile keeps its own state; the result is dropped then.
  pub async fn finish_upload(
    &mut self,
    upload: BlobUpload,
    sent: Result<(), ApiError>,
  ) -> Result<UploadStep, CoreError> {
    let Some((mut pending, index)) = self.current_file(&upload)? else {
      tracing::debug!(conversation_id = %upload.conversation_id, "upload finished for a file that is gone");
      return Ok(UploadStep::Dropped);
    };
    let BlobUpload {
      target,
      maybe_etag,
      progress,
      ..
    } = upload;
    let finished = match target {
      UploadTarget::Whole { slot, attachment } => self
        .confirm_blob(&slot, sent, &mut pending.files[index])
        .await
        .map(|()| PartOutcome::Uploaded(attachment)),
      UploadTarget::Part(target) => {
        // A stored part's ETag was kept by `BlobUpload::sent`.
        let answer = sent.map(|()| maybe_etag);
        self.held_upload_progress = Some(progress);
        self.finish_part(target, answer, &mut pending.files[index]).await
      },
    };
    let confirmed = match finished {
      Ok(PartOutcome::Uploaded(attachment)) => {
        let file = &mut pending.files[index];
        file.maybe_done = Some(attachment);
        file.maybe_slot = None;
        file.maybe_parts = None;
        file.retry = UploadRetry::default();
        self.held_upload_progress = None;
        UploadStep::Uploaded
      },
      Ok(PartOutcome::Stored) => {
        // A part got through: the backoff starts over for the next one.
        pending.files[index].retry = UploadRetry::default();
        UploadStep::PartUploaded
      },
      Err(error) => {
        self.record_upload_error(&mut pending, index, error)?;
        UploadStep::Failed
      },
    };
    save_pending_upload(&self.store, &pending)?;
    self.complete_upload(&pending)?;
    self.remove_staged_copies(&pending);
    Ok(confirmed)
  }

  /// Downloads and decrypts a file into the download directory; progress and the path are in
  /// [`transfer_state`].
  pub async fn download_attachment_to_disk(
    &mut self,
    conversation: ConversationId,
    attachment: AttachmentId,
  ) -> Result<String, CoreError> {
    let download = self.start_download_to_disk(conversation, attachment).await?;
    let saved = download.save().await;
    self.finish_download_to_disk(attachment, saved)
  }

  /// [`Self::download_attachment_to_disk`] for a host that runs the transfer beside its loop: marks
  /// the file as downloading and hands the transfer back ([`DiskDownload::save`], then
  /// [`Self::finish_download_to_disk`]).
  pub async fn start_download_to_disk(
    &mut self,
    conversation: ConversationId,
    attachment: AttachmentId,
  ) -> Result<DiskDownload, CoreError> {
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
    // The cap is enforced on the bytes actually received, not on the sender-declared size.
    match self.start_download(&reference, MAX_DOWNLOAD_BYTES).await {
      Ok(download) => Ok(DiskDownload {
        download,
        maybe_directory: self.settings.maybe_download_dir.clone(),
        maybe_target: None,
      }),
      Err(error) => {
        let failed = TransferState::Failed {
          reason: failure_of(&error),
        };
        set_transfer(&self.store, attachment, &failed)?;
        Err(error)
      },
    }
  }

  /// Records where a download to disk ended up, or why it failed.
  pub fn finish_download_to_disk(
    &mut self,
    attachment: AttachmentId,
    saved: Result<String, CoreError>,
  ) -> Result<String, CoreError> {
    let state = match &saved {
      Ok(path) => TransferState::Local { path: path.clone() },
      Err(error) => TransferState::Failed {
        reason: failure_of(error),
      },
    };
    set_transfer(&self.store, attachment, &state)?;
    saved
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
          maybe_expires_at: None,
          maybe_expires_in_seconds: settings.maybe_expires_in_seconds,
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
      // What the server granted (its defaults and caps), not what was asked for.
      maybe_expires_at: Some(response.expires_at.0),
      maybe_max_downloads: response.maybe_max_downloads,
      has_password: settings.maybe_password.is_some(),
      download_count: 0,
    };
    let mut links = share_links(&self.store, attachment)?;
    links.push(link.clone());
    records::put_json(&self.store, Table::ShareLinks, &attachment.to_string(), &links)?;
    Ok(link)
  }

  /// Stops a share link from working and forgets it. A link the server no longer knows or has
  /// revoked already is gone either way.
  pub async fn revoke_share_link(
    &mut self,
    attachment: AttachmentId,
    share_id: &str,
  ) -> Result<(), CoreError> {
    match self.api.revoke_share_link(share_id).await {
      Ok(()) => {},
      Err(error) if is_status(&error, 409) || is_status(&error, 404) => {},
      Err(error) => return Err(error.into()),
    }
    let mut links = share_links(&self.store, attachment)?;
    links.retain(|link| link.share_id != share_id);
    records::put_json(&self.store, Table::ShareLinks, &attachment.to_string(), &links)?;
    Ok(())
  }

  /// Updates the download counts of the file's links from the server and forgets the ones that were
  /// revoked (maybe by an admin or on another device) or have expired.
  pub async fn refresh_share_links(&mut self, attachment: AttachmentId) -> Result<(), CoreError> {
    let mut links = share_links(&self.store, attachment)?;
    if links.is_empty() {
      return Ok(());
    }
    let listed = self.api.list_share_links().await?.links;
    let now = self.now_ms();
    links.retain_mut(
      |link| match listed.iter().find(|listed| listed.id == link.share_id) {
        Some(listed) if listed.is_revoked || listed.expires_at.0 <= now => false,
        Some(listed) => {
          link.download_count = listed.download_count;
          true
        },
        // The list is capped; a link missing from it is not necessarily gone.
        None => true,
      },
    );
    records::put_json(&self.store, Table::ShareLinks, &attachment.to_string(), &links)?;
    Ok(())
  }

  /// Starts the file's next upload step: a large file's next part, any other file whole.
  async fn start_file(
    &mut self,
    conversation: ConversationId,
    file: &mut PendingFile,
  ) -> Result<(UploadTarget, Option<BlobRequest>, TransferGuard), UploadError> {
    if file.size_bytes > self.settings.max_upload_bytes {
      return Err(UploadError::Failed(TransferFailure::TooLarge));
    }
    if is_multipart(file.size_bytes)
      && let Some(started) = self.start_part(conversation, file).await?
    {
      let maybe_request = started
        .request
        .map(|request| -> BlobRequest { Box::pin(async move { request.await.map(Some) }) });
      return Ok((
        UploadTarget::Part(started.target),
        maybe_request,
        started.progress,
      ));
    }
    let (attachment, request, progress) = self.prepare_blob(conversation, file).await?;
    let Some(slot) = file.maybe_slot.clone() else {
      // Invariant: `prepare_blob` leaves the slot it uploads into on the file.
      return Err(UploadError::Failed(TransferFailure::LocalFile));
    };
    Ok((UploadTarget::Whole { slot, attachment }, Some(request), progress))
  }

  /// Encrypts one file and starts its upload into its slot, announcing one first if it has none.
  /// Keeps the thumbnail and the picture's size on the file for the pending message.
  async fn prepare_blob(
    &self,
    conversation: ConversationId,
    file: &mut PendingFile,
  ) -> Result<(AttachmentRef, BlobRequest, TransferGuard), UploadError> {
    let content = read_source(&file.source, file.size_bytes)
      .await
      .map_err(|()| UploadError::Failed(TransferFailure::LocalFile))?;
    let maybe_thumbnail = crate::thumbnail::make_thumbnail(&content, &file.media_type)
      .await
      .or_else(|| {
        file
          .maybe_thumbnail
          .as_ref()
          .filter(|thumbnail| thumbnail.as_slice().len() <= MAX_APP_THUMBNAIL_BYTES)
          .map(|thumbnail| thumbnail.as_slice().to_vec())
      });
    // Receivers reserve the picture's space before anything loads (issue #316).
    let maybe_dimensions = crate::thumbnail::image_dimensions(&content, &file.media_type)
      .map(|(width, height)| ImageDimensions { width, height });
    if file.maybe_thumbnail.is_none() {
      file.maybe_thumbnail = maybe_thumbnail.clone().map(Base64Bytes::new);
    }
    file.maybe_dimensions = file.maybe_dimensions.or(maybe_dimensions);
    let encrypted = encrypt_attachment(&content, DEFAULT_CHUNK_SIZE, &mut OsRng)
      .map_err(|_| UploadError::Failed(TransferFailure::LocalFile))?;
    let size_bytes = encrypted.ciphertext.len() as u64;
    let slot = match file.maybe_slot.take() {
      Some(slot) if slot.size_bytes == size_bytes => slot,
      _ => self.announce_upload(conversation, size_bytes).await?,
    };
    let progress = self.transfers.start(slot.attachment_id, size_bytes);
    let blob = self
      .api
      .put_blob_detached(&slot.upload_url, encrypted.ciphertext, progress.counter());
    let request: BlobRequest = Box::pin(async move { blob.await.map(|()| None) });
    file.maybe_slot = Some(slot.clone());
    let attachment = AttachmentRef {
      attachment_id: slot.attachment_id,
      key: Base64Bytes::new(encrypted.key.as_key().expose_secret().to_vec()),
      ciphertext_sha256: Base64Bytes::new(encrypted.ciphertext_sha256.to_vec()),
      file_name: file.file_name.clone(),
      media_type: file.media_type.clone(),
      size_bytes: content.len() as u64,
      maybe_thumbnail: maybe_thumbnail.map(Base64Bytes::new),
      maybe_dimensions,
      maybe_duration_ms: file.maybe_duration_ms,
    };
    Ok((attachment, request, progress))
  }

  /// Completes the slot after its blob upload. A storage refusal (expired URL) or an upload that
  /// went missing drops the slot, so the next attempt announces a fresh one.
  async fn confirm_blob(
    &self,
    slot: &UploadSlot,
    sent: Result<(), ApiError>,
    file: &mut PendingFile,
  ) -> Result<(), UploadError> {
    if let Err(error) = sent {
      // Was it the storage or the whole network? Completing the slot tells: it succeeds if the
      // blob arrived after all, fails with "not uploaded" if the server is there, and doesn't reach
      // it when this device is offline.
      match self.api.complete_attachment(slot.attachment_id).await {
        Ok(_) => {},
        Err(probe) if is_unreachable(&probe) => return Err(UploadError::Offline(CoreError::Api(error))),
        // The server says its storage is down and when to come back: its answer decides the wait
        // (#749), and the slot stays for that retry.
        Err(probe) if probe.retry_after().is_some() => return Err(slot_failure(file, probe)),
        Err(_) => return Err(slot_failure(file, error)),
      }
    } else if let Err(error) = self.api.complete_attachment(slot.attachment_id).await {
      if is_unreachable(&error) {
        return Err(UploadError::Offline(CoreError::Api(error)));
      }
      return Err(slot_failure(file, error));
    }
    Ok(())
  }

  /// Records why a file didn't go up: a network error is saved and returned (the search stops), a
  /// file that can never go up is marked failed.
  fn record_upload_error(
    &self,
    upload: &mut PendingUpload,
    index: usize,
    error: UploadError,
  ) -> Result<(), CoreError> {
    match error {
      UploadError::Offline(error) => {
        // Like the outbox: nothing counts while the server can't be reached at all.
        save_pending_upload(&self.store, upload)?;
        Err(error)
      },
      UploadError::Retry(error) => {
        // An expired access token is the app's to renew; it isn't the file's failure.
        if !error.is_sign_in_required() {
          let now = self.now_ms();
          let maybe_server_wait = match &error {
            CoreError::Api(api) => api.retry_after(),
            _ => None,
          };
          upload.files[index].retry.record_failure_after(
            now,
            &self.settings.upload_retry,
            RetryCause::of(&error),
            maybe_server_wait,
          );
        }
        save_pending_upload(&self.store, upload)?;
        Err(error)
      },
      UploadError::Failed(reason) => {
        upload.files[index].maybe_failure = Some(reason);
        save_pending_upload(&self.store, upload)
      },
    }
  }

  /// Announces an upload. Only a server out of reach is "offline" (retried on every tick without
  /// counting, like the outbox). Server trouble (5xx, 429) and too many unfinished uploads (409,
  /// cleared as the server sweeps expired ones) count as failed attempts: backoff, then a visible
  /// stall with "Try again". Only a file the server can never take fails for good.
  async fn announce_upload(
    &self,
    conversation: ConversationId,
    size_bytes: u64,
  ) -> Result<UploadSlot, UploadError> {
    let request = CreateAttachmentRequest {
      conversation_id: conversation,
      size_bytes,
    };
    match self.api.create_attachment(&request).await {
      Ok(created) => Ok(UploadSlot {
        attachment_id: created.id,
        upload_url: created.upload_url,
        size_bytes,
      }),
      Err(error) if is_unreachable(&error) => Err(UploadError::Offline(CoreError::Api(error))),
      Err(error) if error.error_code() == Some(&ErrorCode::PayloadTooLarge) || is_status(&error, 413) => {
        Err(UploadError::Failed(TransferFailure::TooLarge))
      },
      Err(error) => Err(UploadError::Retry(CoreError::Api(error))),
    }
  }

  /// Gives up the uploads the user cancelled at the server; what doesn't get through (offline) is
  /// tried on the next upload step.
  async fn abandon_cancelled_uploads(&mut self) {
    let Ok(Some(abandoned)) =
      records::get_json::<Vec<AttachmentId>>(&self.store, Table::FileTransfers, ABANDONED_KEY)
    else {
      return;
    };
    let mut left = Vec::new();
    for attachment in abandoned {
      match self.api.abandon_attachment(attachment).await {
        Err(error) if error.is_unreachable() => left.push(attachment),
        // A completed one stays (409); any other refusal won't change by asking again.
        Err(error) => tracing::debug!(%error, "a cancelled upload was not given up"),
        Ok(()) => {},
      }
    }
    let saved = if left.is_empty() {
      self
        .store
        .delete(Table::FileTransfers, ABANDONED_KEY)
        .map_err(CoreError::from)
    } else {
      records::put_json(&self.store, Table::FileTransfers, ABANDONED_KEY, &left)
    };
    if let Err(error) = saved {
      tracing::warn!(%error, "the cancelled uploads could not be saved");
    }
  }

  /// Drops the browser's picked files that no pending upload reads any more.
  async fn sweep_picked_files(&mut self) {
    if !self.is_picked_sweep_due {
      return;
    }
    self.is_picked_sweep_due = false;
    let Ok(uploads) = records::list_json::<PendingUpload>(&self.store, Table::PendingUploads) else {
      return;
    };
    let referenced = uploads
      .iter()
      .flat_map(|upload| upload.files.iter())
      .filter_map(|file| match &file.source {
        UploadSource::Picked { handle } => Some(handle.clone()),
        _ => None,
      })
      .collect();
    crate::picked_files::forget_all_but(&referenced).await;
  }

  /// Deletes the app's staged plaintext copies of a finished upload (see
  /// [`super::ClientSettings::maybe_upload_staging_dir`]).
  fn remove_staged_copies(&mut self, upload: &PendingUpload) {
    if !is_finished(upload) {
      return;
    }
    if upload
      .files
      .iter()
      .any(|file| matches!(file.source, UploadSource::Picked { .. }))
    {
      self.is_picked_sweep_due = true;
    }
    let Some(staging) = &self.settings.maybe_upload_staging_dir else {
      return;
    };
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
        // The bytes and the upload's key aren't needed any more, only the name and the reason.
        source: UploadSource::Bytes {
          bytes: Base64Bytes::new(Vec::new()),
        },
        maybe_parts: None,
        ..file.clone()
      })
      .collect();
    if !failed.is_empty() {
      records::put_json(&self.store, Table::FileTransfers, &failed_key(&key), &failed)?;
    }
    for attachment in upload
      .files
      .iter()
      .filter(|file| file.maybe_failure.is_some())
      .filter_map(announced_attachment)
    {
      queue_abandon(&self.store, attachment)?;
    }
    self.store.delete(Table::PendingUploads, &key)?;
    Ok(())
  }
}

impl DiskDownload {
  pub fn attachment_id(&self) -> AttachmentId {
    self.download.attachment_id()
  }

  /// Receives and decrypts the file and writes it into the download directory.
  #[cfg(not(target_arch = "wasm32"))]
  pub async fn save(self) -> Result<String, CoreError> {
    let Self {
      download,
      maybe_directory,
      maybe_target,
    } = self;
    if let Some(path) = maybe_target {
      // The user picked this file in a save dialog, which asked before replacing one.
      let plaintext = download.open().await?;
      std::fs::write(&path, plaintext.expose_secret())
        .map_err(|error| CoreError::Corrupt(error.to_string()))?;
      return Ok(path.to_string_lossy().into_owned());
    }
    let directory =
      maybe_directory.ok_or_else(|| CoreError::Corrupt("no download directory configured".into()))?;
    let file_name = download.reference().file_name.clone();
    let plaintext = download.open().await?;
    std::fs::create_dir_all(&directory).map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let path = unique_path(&directory, &file_name);
    std::fs::write(&path, plaintext.expose_secret())
      .map_err(|error| CoreError::Corrupt(error.to_string()))?;
    Ok(path.to_string_lossy().into_owned())
  }

  /// The browser has no download directory: the decrypted file is handed to the browser through a
  /// temporary `<a download>` link, which saves it into the browser's download folder.
  #[cfg(target_arch = "wasm32")]
  pub async fn save(self) -> Result<String, CoreError> {
    let file_name = sanitize_file_name(&self.download.reference().file_name);
    let plaintext = self.download.open().await?;
    save_in_browser(plaintext.expose_secret(), &file_name)?;
    Ok(file_name)
  }
}

/// A failed blob upload or completion while the server answers: a refusal (expired URL, a blob the
/// storage lost) drops the slot, so the next attempt announces a new one; server trouble (5xx, 429)
/// and a refused connection (CSP, proxy) keep it.
fn slot_failure(file: &mut PendingFile, error: ApiError) -> UploadError {
  let is_refused =
    matches!(&error, ApiError::Status { status, .. } if (400..500).contains(status) && *status != 429);
  if is_refused && !error.is_sign_in_required() {
    tracing::debug!(%error, "upload slot refused; the next attempt announces a new one");
    file.maybe_slot = None;
  }
  UploadError::Retry(CoreError::Api(error))
}

/// The request never got an answer: this device is offline, the server is out of reach, or a
/// captive portal or proxy answered instead.
fn is_unreachable(error: &ApiError) -> bool {
  error.is_unreachable()
}

/// A file on its way to disk (or, in the browser, to its download folder), detached from the client
/// like a [`BlobDownload`].
pub struct DiskDownload {
  download: BlobDownload,
  #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
  maybe_directory: Option<std::path::PathBuf>,
  /// The exact file to write (picked in a save dialog) instead of a new name in the directory.
  #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
  maybe_target: Option<std::path::PathBuf>,
}

impl DiskDownload {
  /// Writes the file to `path` (the user's choice in a save dialog) instead of the download
  /// directory. The browser has no paths and saves to its download folder anyway.
  pub fn to_path(self, path: std::path::PathBuf) -> Self {
    Self {
      maybe_target: Some(path),
      ..self
    }
  }
}

/// What became of a finished blob upload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadStep {
  /// The file is up and in its message.
  Uploaded,
  /// One part of a large file is up; the next follows on the next step.
  PartUploaded,
  /// It failed for good (shown on the message).
  Failed,
  /// The message was deleted or its upload retried meanwhile: nothing changed.
  Dropped,
}

impl BlobUpload {
  /// Resolves once storage answered. Polling may stop half way (a host's `select`) and resume on
  /// the next call; after it resolved, the upload goes to [`Client::finish_upload`].
  pub async fn sent(&mut self) -> Result<(), ApiError> {
    let answer = match &mut self.maybe_request {
      Some(request) => request.await,
      None => Ok(None),
    };
    self.maybe_request = None;
    // A part's ETag stays here for `finish_upload`; an error goes to the host, which hands it on.
    self.maybe_etag = answer?;
    Ok(())
  }
}

impl UploadTarget {
  /// Whether `file` is the one this upload sends: by its slot, or by the part that comes next.
  fn is_sending(&self, file: &PendingFile) -> bool {
    match self {
      Self::Whole { slot, .. } => file.maybe_slot.as_ref() == Some(slot),
      Self::Part(target) => file.maybe_parts.as_ref().is_some_and(|parts| {
        parts.attachment_id == target.attachment_id
          && target
            .maybe_part
            .as_ref()
            .is_none_or(|part| usize::from(part.part_number) == parts.etags.len() + 1)
      }),
    }
  }
}

/// A blob or part request beside the host's loop; a part answers with its `ETag`.
type BlobRequest = Pin<Box<dyn Future<Output = Result<Option<String>, ApiError>>>>;

/// The first file of `upload` that is neither done nor failed and whose attempt is due.
fn next_due_file(upload: &PendingUpload, now_ms: i64) -> Option<usize> {
  upload
    .files
    .iter()
    .position(|file| file.maybe_done.is_none() && file.maybe_failure.is_none() && file.retry.is_due(now_ms))
}

pub(super) enum UploadError {
  /// The server can't be reached: keep the file pending, without counting an attempt.
  Offline(CoreError),
  /// The upload itself failed (refused blob, lost slot, 409): try again after a backoff.
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

/// The user's "Retry" on a file that stopped retrying by itself: every stalled file of the message is
/// due again at once, with a fresh count. Returns whether the message had such files.
pub fn retry_stalled_upload(
  store: &impl LocalStore,
  conversation: ConversationId,
  message: ClientMessageId,
) -> Result<bool, CoreError> {
  let Some(mut upload) = pending_upload(store, conversation, message)? else {
    return Ok(false);
  };
  let mut has_waiting = false;
  for file in &mut upload.files {
    if file.maybe_done.is_none() && file.maybe_failure.is_none() && file.retry.is_stalled() {
      file.retry = UploadRetry::default();
      has_waiting = true;
    }
  }
  if has_waiting {
    save_pending_upload(store, &upload)?;
  }
  Ok(has_waiting)
}

/// The user's "Cancel" on a file still waiting to go up: the message goes out without it. A message
/// left with neither text nor files is dropped from the outbox. Returns whether a file was removed.
pub fn cancel_pending_file(
  store: &impl LocalStore,
  conversation: ConversationId,
  message: ClientMessageId,
  index: usize,
) -> Result<bool, CoreError> {
  let Some(mut upload) = pending_upload(store, conversation, message)? else {
    return Ok(false);
  };
  if upload
    .files
    .get(index)
    .is_none_or(|file| file.maybe_done.is_some())
  {
    return Ok(false);
  }
  let file = upload.files.remove(index);
  if let Some(attachment) = announced_attachment(&file) {
    queue_abandon(store, attachment)?;
  }
  let has_text = store.message(conversation, message)?.is_some_and(
    |stored| matches!(&stored.content, MessageContent::Text { body, .. } if !body.trim().is_empty()),
  );
  if !upload.files.is_empty() || has_text {
    // Completed by the host's next upload step when the rest is up.
    save_pending_upload(store, &upload)?;
    return Ok(true);
  }
  crate::store::write_group(store, || {
    let key = records::outbox_key(conversation, message);
    store.delete(Table::PendingUploads, &key)?;
    store.delete(Table::Outbox, &key)?;
    // Never sent: nobody else has it, so nothing of it is left in the conversation.
    store.delete_message(conversation, message)?;
    Ok::<_, CoreError>(())
  })?;
  Ok(true)
}

/// The attachment the server announced for a file that isn't up yet, if any.
fn announced_attachment(file: &PendingFile) -> Option<AttachmentId> {
  file
    .maybe_parts
    .as_ref()
    .map(|parts| parts.attachment_id)
    .or_else(|| file.maybe_slot.as_ref().map(|slot| slot.attachment_id))
}

pub(super) fn queue_abandon(store: &impl LocalStore, attachment: AttachmentId) -> Result<(), CoreError> {
  let mut abandoned: Vec<AttachmentId> =
    records::get_json(store, Table::FileTransfers, ABANDONED_KEY)?.unwrap_or_default();
  if !abandoned.contains(&attachment) {
    abandoned.push(attachment);
    records::put_json(store, Table::FileTransfers, ABANDONED_KEY, &abandoned)?;
  }
  Ok(())
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
      UploadSource::Bytes { .. } | UploadSource::Picked { .. } => None,
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

async fn read_source(source: &UploadSource, size_bytes: u64) -> Result<Vec<u8>, ()> {
  match source {
    UploadSource::Bytes { bytes } => Ok(bytes.as_slice().to_vec()),
    #[cfg(not(target_arch = "wasm32"))]
    UploadSource::Path { path } => std::fs::read(path).map_err(|error| {
      tracing::warn!(%error, "could not read a file to upload");
    }),
    #[cfg(target_arch = "wasm32")]
    UploadSource::Path { .. } => Err(()),
    UploadSource::Picked { handle } => crate::picked_files::read(handle, 0, size_bytes, size_bytes)
      .await
      .map(|bytes| bytes.to_vec())
      .ok_or(()),
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

pub(super) fn is_status(error: &ApiError, status: u16) -> bool {
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
        maybe_slot: None,
        maybe_parts: None,
        retry: UploadRetry::default(),
        maybe_thumbnail: None,
        maybe_dimensions: None,
        maybe_duration_ms: None,
      }],
    };
    save_pending_upload(&store, &upload).unwrap();

    assert_eq!(sweep_staged_copies(&store, &staging).unwrap(), 1);
    assert!(pending.exists(), "still to be uploaded");
    assert!(!stray.exists());
    assert_eq!(sweep_staged_copies(&store, &staging.join("missing")).unwrap(), 0);
    std::fs::remove_dir_all(&staging).unwrap();
  }

  mod upload_retry {
    use super::*;

    #[test]
    fn waits_double_up_to_the_ceiling_and_stop_at_the_cap() {
      let settings = UploadRetrySettings {
        retry_after: std::time::Duration::from_secs(2),
        retry_max: std::time::Duration::from_secs(10),
        max_attempts: 5,
      };
      let mut retry = UploadRetry::default();
      assert!(retry.is_due(0));
      let waits: Vec<i64> = (0..4)
        .map(|_| {
          retry.record_failure(1_000, &settings, RetryCause::Network);
          let UploadRetry::Waiting { retry_at_ms, .. } = retry else {
            panic!("still retrying: {retry:?}");
          };
          retry_at_ms - 1_000
        })
        .collect();
      assert_eq!(
        waits,
        vec![2_000, 4_000, 8_000, 10_000],
        "doubling up to the ceiling"
      );
      assert!(!retry.is_due(10_999));
      assert!(retry.is_due(11_000));
      retry.record_failure(1_000, &settings, RetryCause::Network);
      assert_eq!(
        retry,
        UploadRetry::Stalled {
          attempts: 5,
          cause: RetryCause::Network
        }
      );
      assert!(!retry.is_due(i64::MAX), "a stalled file waits for the user");
    }

    #[test]
    fn the_defaults_give_up_after_about_a_minute_never_endlessly() {
      let settings = UploadRetrySettings::default();
      let mut retry = UploadRetry::default();
      let mut now = 0;
      let mut waited = 0;
      while let UploadRetry::Fresh | UploadRetry::Waiting { .. } = retry {
        retry.record_failure(now, &settings, RetryCause::StorageUnavailable);
        if let UploadRetry::Waiting { retry_at_ms, .. } = retry {
          waited += retry_at_ms - now;
          now = retry_at_ms;
        }
      }
      assert_eq!(retry.failed_attempts(), settings.max_attempts);
      assert_eq!(waited, 62_000, "2 + 4 + 8 + 16 + 32 seconds");
      assert_eq!(retry.stall_cause(), Some(RetryCause::StorageUnavailable));
    }

    #[test]
    fn the_servers_retry_after_is_honoured_but_capped() {
      let settings = UploadRetrySettings::default();
      let wait_after = |maybe_server_wait: Option<u64>| {
        let mut retry = UploadRetry::default();
        retry.record_failure_after(
          0,
          &settings,
          RetryCause::StorageUnavailable,
          maybe_server_wait.map(std::time::Duration::from_secs),
        );
        match retry {
          UploadRetry::Waiting { retry_at_ms, .. } => retry_at_ms,
          other => panic!("still retrying: {other:?}"),
        }
      };
      assert_eq!(wait_after(None), 2_000, "the backoff alone");
      assert_eq!(wait_after(Some(30)), 30_000, "the server asked for 30 s");
      assert_eq!(wait_after(Some(1)), 2_000, "never sooner than the backoff");
      assert_eq!(wait_after(Some(3_600)), 60_000, "never longer than retry_max");
    }

    #[test]
    fn the_last_cause_is_the_one_the_stall_shows() {
      let settings = UploadRetrySettings {
        max_attempts: 2,
        ..UploadRetrySettings::default()
      };
      let mut retry = UploadRetry::default();
      retry.record_failure(0, &settings, RetryCause::StorageUnavailable);
      assert_eq!(retry.stall_cause(), None, "still retrying");
      retry.record_failure(0, &settings, RetryCause::Network);
      assert_eq!(retry.stall_cause(), Some(RetryCause::Network));
    }

    #[test]
    fn a_503_or_the_unavailable_code_is_the_storage_being_down() {
      let unavailable = CoreError::Api(ApiError::status(503, ErrorCode::Unavailable, "storage"));
      assert_eq!(RetryCause::of(&unavailable), RetryCause::StorageUnavailable);
      let bare_503 = CoreError::Api(ApiError::Status {
        status: 503,
        maybe_body: None,
        maybe_retry_after: None,
      });
      assert_eq!(RetryCause::of(&bare_503), RetryCause::StorageUnavailable);
      let conflict = CoreError::Api(ApiError::status(409, ErrorCode::Conflict, "not uploaded"));
      assert_eq!(RetryCause::of(&conflict), RetryCause::Network);
      let offline = CoreError::Api(ApiError::Network("reset".into()));
      assert_eq!(RetryCause::of(&offline), RetryCause::Network);
    }

    #[test]
    fn stored_retries_without_a_cause_still_read_and_keep_their_form() {
      let older: UploadRetry =
        serde_json::from_str(r#"{"state":"stalled","attempts":8}"#).expect("an older record parses");
      assert_eq!(older.stall_cause(), Some(RetryCause::Network));
      assert_eq!(
        serde_json::to_string(&older).expect("serialize"),
        r#"{"state":"stalled","attempts":8}"#
      );
      let unavailable = UploadRetry::Stalled {
        attempts: 6,
        cause: RetryCause::StorageUnavailable,
      };
      assert_eq!(
        serde_json::to_string(&unavailable).expect("serialize"),
        r#"{"state":"stalled","attempts":6,"cause":"storage_unavailable"}"#
      );
    }

    #[test]
    fn a_video_duration_is_stored_and_older_files_have_none() {
      let mut file: PendingFile = serde_json::from_value(serde_json::json!({
        "file_name": "a.mp4",
        "media_type": "video/mp4",
        "source": { "kind": "bytes", "bytes": "YQ==" },
        "size_bytes": 1,
      }))
      .unwrap();
      assert_eq!(file.maybe_duration_ms, None, "stored before the field existed");
      assert!(serde_json::to_value(&file).unwrap().get("duration_ms").is_none());
      file.maybe_duration_ms = Some(12_500);
      let json = serde_json::to_value(&file).unwrap();
      assert_eq!(json["duration_ms"], 12_500);
      let again: PendingFile = serde_json::from_value(json).unwrap();
      assert_eq!(again.maybe_duration_ms, Some(12_500));
    }

    #[test]
    fn a_fresh_retry_is_not_stored() {
      let file = PendingFile {
        file_name: "a.txt".into(),
        media_type: "text/plain".into(),
        source: UploadSource::Bytes {
          bytes: Base64Bytes::new(b"a".to_vec()),
        },
        size_bytes: 1,
        maybe_done: None,
        maybe_failure: None,
        maybe_slot: None,
        maybe_parts: None,
        retry: UploadRetry::default(),
        maybe_thumbnail: None,
        maybe_dimensions: None,
        maybe_duration_ms: None,
      };
      let json = serde_json::to_value(&file).unwrap();
      assert!(json.get("retry").is_none() && json.get("slot").is_none());
      // Files queued by an older version have neither.
      let old: PendingFile = serde_json::from_value(serde_json::json!({
        "file_name": "a.txt", "media_type": "text/plain", "size_bytes": 1,
        "source": { "kind": "bytes", "bytes": "YQ==" }
      }))
      .unwrap();
      assert_eq!(old, file);
    }
  }
}
