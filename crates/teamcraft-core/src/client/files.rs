//! Files beyond single attachments: sending text with several files (uploads resume per file
//! after a restart or network loss), downloading to disk, and share links (e2ee.md §4.5.1).

use std::pin::Pin;

use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use teamcraft_crypto::attachment::{
  AttachmentKey, DEFAULT_CHUNK_SIZE, EncryptedAttachment, encrypt_attachment, encrypted_len,
};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_crypto::share_link::create_fragment;
use teamcraft_proto::{
  AttachmentId, Base64Bytes, ClientMessageId, ConversationId, CreateAttachmentRequest,
  CreateShareLinkRequest, ErrorCode, ThreadId,
};

use super::Client;
#[cfg(target_arch = "wasm32")]
use super::attachments::BlobDownload;
use super::attachments::OutgoingFile;
use super::disk_downloads::{self, DownloadTarget};
use super::upload_parts::{PartOutcome, PartTarget, PartsUpload, is_multipart};
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::records;
use crate::retry::{Jitter, JitterMode, RetryPolicy, millis};
use crate::store::{AttachmentRef, ImageDimensions, LocalStore, MessageContent, MessageFilter, Table};
use crate::transfers::TransferGuard;

/// Argon2id for share link passwords: the browser download page runs it in WebAssembly, so it is
/// lighter than the backup passphrase's (the link itself is the main secret).
const SHARE_LINK_ARGON2: Argon2Settings = Argon2Settings {
  memory_kib: 32 * 1024,
  iterations: 3,
  parallelism: 1,
};

/// Files of an outbox message that aren't uploaded yet; the message waits until they are. A file
/// attached to a draft (#1220) waits here too, under its own ID, until it is up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingUpload {
  pub conversation_id: ConversationId,
  /// The message's ID, or for a draft's file the file's ID.
  pub client_message_id: ClientMessageId,
  pub files: Vec<PendingFile>,
  #[serde(default, skip_serializing_if = "UploadPurpose::is_message")]
  pub purpose: UploadPurpose,
}

/// What an upload is for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UploadPurpose {
  /// The files of a message in the outbox.
  #[default]
  Message,
  /// One file attached to a draft (#1220). Once up it moves into the account data, so the draft's
  /// other devices have it; until then (or for good, see `is_local`) it is this device's.
  Draft {
    #[serde(rename = "thread_id", default, skip_serializing_if = "Option::is_none")]
    maybe_thread: Option<ThreadId>,
    added_at_ms: i64,
    /// Stays on this device until it is sent: the drafts already carried as many files as travel
    /// (`account_data::MAX_DRAFT_FILES`) when it was attached.
    #[serde(default)]
    is_local: bool,
  },
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
  /// A voice message's waveform, from the sender's app; it goes into the attachment's details.
  #[serde(rename = "waveform", default, skip_serializing_if = "Option::is_none")]
  pub maybe_waveform: Option<Base64Bytes>,
  /// A large file's upload in parts once announced: a restart continues at its next part.
  #[serde(rename = "parts", default, skip_serializing_if = "Option::is_none")]
  pub maybe_parts: Option<PartsUpload>,
  /// Announced as a draft's file (#1220): the server holds it for the draft, and deletes it later
  /// unless it is kept. A message keeps its held files before it goes out.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub is_held: bool,
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
pub(super) enum UploadTarget {
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
  /// When the presigned URL stops working, by the server's clock. Absent in slots stored before it
  /// was kept.
  #[serde(rename = "expires_at_ms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at_ms: Option<i64>,
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
  /// Stopped retrying at the usual pace and says so, with a "Retry" for the user
  /// ([`retry_stalled_upload`]). Still tried now and then, so an outage that ends sends the file
  /// without anybody having to notice.
  Stalled {
    attempts: u32,
    #[serde(default, skip_serializing_if = "RetryCause::is_network")]
    cause: RetryCause,
    /// When it is tried again by itself (client clock). Absent in records stored before this was
    /// kept: tried at once, and from then on it is set.
    #[serde(rename = "retry_at_ms", default, skip_serializing_if = "Option::is_none")]
    maybe_retry_at_ms: Option<i64>,
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

/// How uploads and downloads that failed on the network are retried, on the shared policy
/// ([`crate::retry`], #743). Configurable; by default an upload is shown as stopped after about a
/// minute of failures (2, 4, 8, 16 and 32 seconds apart, each spread by jitter so devices that lost
/// the storage together don't come back together), with why and a "Retry" (#742). It goes on being
/// tried every few minutes, so nobody has to press it once an outage is over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadRetrySettings {
  /// Wait after the first failed attempt; doubles with each further one.
  pub retry_after: std::time::Duration,
  /// The wait between attempts grows up to this.
  pub retry_max: std::time::Duration,
  /// Failed attempts after which the file is shown as stopped, with "Retry".
  pub max_attempts: u32,
  /// How often a stopped upload is still tried by itself (spread by the jitter).
  pub stalled_retry_every: std::time::Duration,
  /// How the wait is spread; tests that step a clock by the backoff turn it off.
  pub jitter: JitterMode,
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
      stalled_retry_every: std::time::Duration::from_secs(5 * 60),
      jitter: JitterMode::Equal,
    }
  }
}

impl UploadSlot {
  /// Whether the upload URL no longer works. Only ever asked of a slot waiting for a retry; with
  /// this device's clock running ahead, the next attempt merely announces a new slot.
  fn has_expired(&self, now_ms: i64) -> bool {
    self
      .maybe_expires_at_ms
      .is_some_and(|expires_at| now_ms >= expires_at)
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
      Self::Stalled {
        maybe_retry_at_ms, ..
      } => maybe_retry_at_ms.is_none_or(|retry_at_ms| now_ms >= retry_at_ms),
    }
  }

  /// Counts a failed attempt: the next one waits with exponential backoff, and after
  /// [`UploadRetrySettings::max_attempts`] the file stalls, keeping `cause` for the user and being
  /// tried only every [`UploadRetrySettings::stalled_retry_every`].
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
      let policy = RetryPolicy::new(settings.stalled_retry_every, settings.stalled_retry_every)
        .with_jitter(settings.jitter);
      let wait = millis(policy.delay(0, &mut Jitter::new(OsRng.next_u64()), maybe_server_wait));
      *self = Self::Stalled {
        attempts,
        cause,
        maybe_retry_at_ms: Some(now_ms.saturating_add(wait)),
      };
      return;
    }
    let policy = RetryPolicy::new(settings.retry_after, settings.retry_max).with_jitter(settings.jitter);
    let wait = millis(policy.delay(
      attempts.saturating_sub(1),
      &mut Jitter::new(OsRng.next_u64()),
      maybe_server_wait,
    ));
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
  /// What `error` says about the attempt: 502, 503 and 504 (the storage or a gateway in front of it
  /// answers for an upload sent straight to it) or the server's `unavailable` code mean the storage
  /// or the server is down for a while; anything else counts as a network failure.
  pub fn of(error: &CoreError) -> Self {
    match error {
      CoreError::Api(api)
        if [502, 503, 504].into_iter().any(|status| is_status(api, status))
          || api.error_code() == Some(&ErrorCode::Unavailable) =>
      {
        Self::StorageUnavailable
      },
      _ => Self::Network,
    }
  }

  fn is_network(&self) -> bool {
    *self == Self::Network
  }
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

/// Numbered names tried for a saved file whose name is taken (`name (2)` … `name (N)`).
#[cfg(not(target_arch = "wasm32"))]
const MAX_NAME_ATTEMPTS: usize = 10_000;

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

/// Why a file didn't make it, kept with the file so its card can say so (issue #807). Stored in the
/// local store: variants are only added.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferFailure {
  TooLarge,
  /// The transfer failed on the network, or the server or the storage answered with an error.
  Network,
  /// The blob doesn't match what the sender encrypted (tampered or corrupted): not decrypted.
  Corrupted,
  /// Reading or writing the file on this device failed.
  LocalFile,
  /// The file storage (or the server) answered "unavailable" (503): an outage that a later retry
  /// gets through.
  StorageUnavailable,
  /// The server doesn't have the file (any more), or doesn't give it to this user (404).
  Missing,
  /// A captive portal answered, or the connection's certificate isn't trusted.
  Intercepted,
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
          maybe_waveform: None,
          is_held: false,
        })
        .collect(),
      purpose: UploadPurpose::Message,
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
    let can_hold_drafts = self.draft_file_retention_days().is_some();
    'uploads: for mut upload in pending {
      match upload.purpose {
        // Goes up with its message, when it is sent; the composer shows its thumbnail meanwhile.
        UploadPurpose::Draft { is_local: true, .. } => {
          self.preview_waiting_draft_file(&mut upload).await?;
          continue;
        },
        UploadPurpose::Draft { .. } if !can_hold_drafts => continue,
        UploadPurpose::Draft { .. } => upload.files.iter_mut().for_each(|file| file.is_held = true),
        UploadPurpose::Message => self.keep_held_files(&mut upload).await?,
      }
      while let Some(index) = next_due_file(&upload, self.now_ms()) {
        let started = self
          .start_file(upload.conversation_id, &mut upload.files[index])
          .await;
        // A draft's file removed while it was being prepared stays removed (#1220).
        if self.is_removed_draft_upload(&upload)? {
          continue 'uploads;
        }
        match started {
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
    // A draft's file removed while its blob was confirmed stays removed (#1220).
    if self.is_removed_draft_upload(&pending)? {
      return Ok(UploadStep::Dropped);
    }
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
    self
      .start_download_to_disk_at(conversation, attachment, DownloadTarget::DownloadDirectory)
      .await
  }

  /// [`Self::start_download_to_disk`] into `target`. The download is recorded
  /// ([`disk_downloads::DownloadRecord`]): natively it continues where it stopped after a network
  /// failure or a restart.
  pub async fn start_download_to_disk_at(
    &mut self,
    conversation: ConversationId,
    attachment: AttachmentId,
    target: DownloadTarget,
  ) -> Result<DiskDownload, CoreError> {
    let reference = find_attachment(&self.store, conversation, attachment)?
      .ok_or_else(|| CoreError::Corrupt("no such file in this conversation".into()))?;
    disk_downloads::record_start(&self.store, conversation, &reference, &target, self.now_ms())?;
    if exceeds_download_limit(reference.size_bytes, MAX_DOWNLOAD_BYTES) {
      let error = CoreError::InvalidRequest("the file is too large to download here".into());
      let too_large = Err(CoreError::Api(ApiError::BlobTooLarge {
        max_bytes: MAX_DOWNLOAD_BYTES.unwrap_or(u64::MAX),
      }));
      self.finish_download_to_disk(attachment, too_large)?;
      return Err(error);
    }
    set_transfer(
      &self.store,
      attachment,
      &TransferState::Downloading {
        total_bytes: reference.size_bytes,
      },
    )?;
    match self.start_disk_transfer(&reference).await {
      Ok(download) => Ok(DiskDownload::new(
        download,
        &target,
        self.settings.maybe_download_dir.clone(),
      )),
      Err(error) => {
        let failed = Err(error);
        self.finish_download_to_disk(attachment, failed)?;
        Err(CoreError::InvalidRequest("the download could not start".into()))
      },
    }
  }

  /// Records where a download to disk ended up, or why it failed.
  pub fn finish_download_to_disk(
    &mut self,
    attachment: AttachmentId,
    saved: Result<String, CoreError>,
  ) -> Result<String, CoreError> {
    let state = disk_downloads::record_finish(
      &self.store,
      attachment,
      &saved,
      self.now_ms(),
      &self.settings.upload_retry,
    )?;
    set_transfer(&self.store, attachment, &state)?;
    saved
  }

  /// The user paused (`Paused`) or cancelled (`Cancelled`) an unfinished download to disk; the
  /// host stopped its transfer. A cancelled one's partial file is deleted, a paused one continues
  /// from it when saved again. The file's card goes back to "on the server".
  pub fn stop_download_to_disk(
    &mut self,
    attachment: AttachmentId,
    status: disk_downloads::DownloadStatus,
  ) -> Result<(), CoreError> {
    let is_cancel = status == disk_downloads::DownloadStatus::Cancelled;
    let Some(before) = disk_downloads::record_stop(&self.store, attachment, status)? else {
      return Ok(());
    };
    #[cfg(not(target_arch = "wasm32"))]
    if is_cancel
      && let Some(partial) = &before.maybe_partial_path
      && let Err(error) = std::fs::remove_file(partial)
      && error.kind() != std::io::ErrorKind::NotFound
    {
      tracing::debug!(%error, "a cancelled download's partial file could not be deleted");
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (is_cancel, before);
    Ok(self.store.delete(Table::FileTransfers, &attachment.to_string())?)
  }

  /// Requests the file's blob: natively from where its partial file ends, in the browser whole.
  #[cfg(not(target_arch = "wasm32"))]
  async fn start_disk_transfer(&self, reference: &AttachmentRef) -> Result<DiskTransfer, CoreError> {
    // The apps always have a staging directory; tools and tests without one keep partial files in
    // the download directory, hidden, or in the temporary directory.
    let staging = match (
      &self.settings.maybe_upload_staging_dir,
      &self.settings.maybe_download_dir,
    ) {
      (Some(staging), _) => staging.clone(),
      (None, Some(directory)) => directory.join(".teamcraft"),
      (None, None) => std::env::temp_dir().join("teamcraft"),
    };
    let download = self.api.download_attachment(reference.attachment_id).await?;
    let progress = self
      .transfers
      .start(reference.attachment_id, reference.size_bytes);
    let partial = disk_downloads::native::partial_path(&staging, reference.attachment_id);
    disk_downloads::record_partial(&self.store, reference.attachment_id, &partial.to_string_lossy())?;
    Ok(disk_downloads::RangedDownload::start(
      reference.clone(),
      partial,
      progress,
      |offset| self.api.get_blob_from_detached(&download.download_url, offset),
    ))
  }

  #[cfg(target_arch = "wasm32")]
  async fn start_disk_transfer(&self, reference: &AttachmentRef) -> Result<DiskTransfer, CoreError> {
    self.start_download(reference, MAX_DOWNLOAD_BYTES).await
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

  /// Whether the file is too large: its encrypted size against the limit the server published
  /// (the plain size against the default for servers that don't), and its plain size against the
  /// host's own cap, if it set one.
  pub(super) fn is_too_large(&self, size_bytes: u64) -> bool {
    let is_over_server = match self.server_attachment_limit() {
      Some(limit) => encrypted_len(size_bytes, DEFAULT_CHUNK_SIZE) > limit,
      None => size_bytes > super::DEFAULT_MAX_UPLOAD_BYTES,
    };
    is_over_server || self.settings.max_upload_bytes.is_some_and(|cap| size_bytes > cap)
  }

  /// The attachment limit the server published, if it does.
  fn server_attachment_limit(&self) -> Option<u64> {
    records::get_json::<Option<u64>>(&self.store, Table::Account, super::SERVER_ATTACHMENT_LIMIT_KEY)
      .ok()
      .flatten()
      .flatten()
  }

  /// Keeps the attachment limit the server published (`None`: it doesn't, as older servers).
  pub(super) fn store_attachment_limit(&self, maybe_limit: Option<u64>) -> Result<(), CoreError> {
    let recorded =
      records::get_json::<Option<u64>>(&self.store, Table::Account, super::SERVER_ATTACHMENT_LIMIT_KEY)?;
    if recorded != Some(maybe_limit) {
      records::put_json(
        &self.store,
        Table::Account,
        super::SERVER_ATTACHMENT_LIMIT_KEY,
        &maybe_limit,
      )?;
    }
    Ok(())
  }

  /// Reads the server's attachment limit and whether it holds drafts' files again (a full sync
  /// does), from the first reads when they have it: one more round trip there costs every start.
  pub(super) async fn refresh_attachment_limit(&mut self) -> Result<(), CoreError> {
    let me = match self.read_ahead.me.take() {
      Some(read) => read?,
      None => self.api.me().await?,
    };
    self.store_draft_limits(me.maybe_draft_file_retention_days, me.maybe_max_key_bag_bytes)?;
    self.store_attachment_limit(me.maybe_max_attachment_bytes)
  }

  /// Starts the file's next upload step: a large file's next part, any other file whole.
  pub(super) async fn start_file(
    &mut self,
    conversation: ConversationId,
    file: &mut PendingFile,
  ) -> Result<(UploadTarget, Option<BlobRequest>, TransferGuard), UploadError> {
    if self.is_too_large(file.size_bytes) {
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
    let _step = crate::timing::step("file_encrypt");
    let SealedFile {
      content_len,
      maybe_made,
      maybe_dimensions,
      encrypted,
    } = seal_file(file, self.settings.makes_thumbnails)
      .await
      .map_err(|()| UploadError::Failed(TransferFailure::LocalFile))?;
    let maybe_thumbnail = maybe_made.or_else(|| {
      file
        .maybe_thumbnail
        .as_ref()
        .filter(|thumbnail| thumbnail.as_slice().len() <= MAX_APP_THUMBNAIL_BYTES)
        .map(|thumbnail| thumbnail.as_slice().to_vec())
    });
    if file.maybe_thumbnail.is_none() {
      file.maybe_thumbnail = maybe_thumbnail.clone().map(Base64Bytes::new);
    }
    file.maybe_dimensions = file.maybe_dimensions.or(maybe_dimensions);
    let size_bytes = encrypted.ciphertext.len() as u64;
    let slot = match file.maybe_slot.take() {
      // A URL that expired while the file waited out an outage would only cost one more round.
      Some(slot) if slot.size_bytes == size_bytes && !slot.has_expired(self.now_ms()) => slot,
      _ => {
        self
          .announce_upload(conversation, size_bytes, file.is_held)
          .await?
      },
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
      size_bytes: content_len,
      maybe_thumbnail: maybe_thumbnail.map(Base64Bytes::new),
      maybe_dimensions,
      maybe_duration_ms: file.maybe_duration_ms,
      maybe_waveform: file.maybe_waveform.clone(),
    };
    Ok((attachment, request, progress))
  }

  /// Completes the slot after its blob upload. A storage refusal (expired URL) or an upload that
  /// went missing drops the slot, so the next attempt announces a fresh one.
  pub(super) async fn confirm_blob(
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
          let maybe_server_wait = crate::retry::server_wait(&error);
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
    is_draft: bool,
  ) -> Result<UploadSlot, UploadError> {
    let request = CreateAttachmentRequest {
      conversation_id: conversation,
      size_bytes,
      is_draft,
    };
    match self.api.create_attachment(&request).await {
      Ok(created) => Ok(UploadSlot {
        attachment_id: created.id,
        upload_url: created.upload_url,
        size_bytes,
        maybe_expires_at_ms: Some(created.expires_at.0),
      }),
      Err(error) if is_unreachable(&error) => Err(UploadError::Offline(CoreError::Api(error))),
      Err(error) if error.error_code() == Some(&ErrorCode::PayloadTooLarge) || is_status(&error, 413) => {
        Err(UploadError::Failed(TransferFailure::TooLarge))
      },
      Err(error) => Err(UploadError::Retry(CoreError::Api(error))),
    }
  }

  /// Makes the thumbnail and reads the size of a picture that waits on this device for its send
  /// (#1585), so the composer shows it, also after a restart. Nothing is announced or uploaded. A
  /// file that isn't a picture the core reads keeps waiting as it is.
  async fn preview_waiting_draft_file(&mut self, upload: &mut PendingUpload) -> Result<(), CoreError> {
    let Some(file) = upload.files.first_mut() else {
      return Ok(());
    };
    if !crate::thumbnail::is_image(&file.media_type)
      || !self.settings.makes_thumbnails
      || file.maybe_thumbnail.is_some()
      || !self.previewed_draft_files.insert(upload.client_message_id)
    {
      return Ok(());
    }
    let Some(preview) = preview_picture(file).await else {
      return Ok(());
    };
    file.maybe_dimensions = file.maybe_dimensions.or(Some(preview.dimensions));
    file.maybe_thumbnail = preview.maybe_thumbnail.map(Base64Bytes::new);
    // Removed or sent while the thumbnail was made: not brought back.
    if pending_upload(&self.store, upload.conversation_id, upload.client_message_id)?.is_some() {
      save_pending_upload(&self.store, upload)?;
    }
    Ok(())
  }

  /// Whether `upload` is a draft's file the user removed meanwhile (its record is gone). What the
  /// server announced for it in the meantime is given up.
  fn is_removed_draft_upload(&self, upload: &PendingUpload) -> Result<bool, CoreError> {
    if upload.purpose.is_message()
      || pending_upload(&self.store, upload.conversation_id, upload.client_message_id)?.is_some()
    {
      return Ok(false);
    }
    for attachment in upload
      .files
      .iter()
      .filter_map(super::draft_files::file_attachment)
    {
      queue_abandon(&self.store, attachment)?;
    }
    Ok(true)
  }

  /// Keeps the drafts' files of a message at the server before it goes out (#1220), so they aren't
  /// deleted with the draft later. A file the server no longer has (removed from the draft on
  /// another device, or not sent in time) fails, and the message goes without it. Any other refusal
  /// backs off on that file like a failed upload, so one message never holds up the others; only a
  /// server out of reach (or a sign-in to renew) stops the round.
  async fn keep_held_files(&mut self, upload: &mut PendingUpload) -> Result<(), CoreError> {
    let now = self.now_ms();
    let mut is_changed = false;
    let mut outcome = Ok(());
    for file in upload
      .files
      .iter_mut()
      .filter(|file| file.is_held && file.maybe_failure.is_none() && file.retry.is_due(now))
    {
      let Some(attachment) = file.maybe_done.as_ref().map(|done| done.attachment_id) else {
        continue;
      };
      match self.api.keep_attachment(attachment).await {
        Ok(()) => {
          file.is_held = false;
          file.retry = UploadRetry::default();
        },
        Err(error) if is_status(&error, 404) => {
          file.is_held = false;
          file.maybe_done = None;
          file.maybe_failure = Some(TransferFailure::Missing);
        },
        Err(error) if is_unreachable(&error) || error.is_sign_in_required() => {
          outcome = Err(CoreError::Api(error));
          break;
        },
        Err(error) => {
          let error = CoreError::Api(error);
          tracing::debug!(%error, "a draft's file was not kept; tried again later");
          let maybe_server_wait = crate::retry::server_wait(&error);
          file.retry.record_failure_after(
            now,
            &self.settings.upload_retry,
            RetryCause::of(&error),
            maybe_server_wait,
          );
        },
      }
      is_changed = true;
    }
    if is_changed {
      save_pending_upload(&self.store, upload)?;
    }
    outcome
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
    if let UploadPurpose::Draft {
      maybe_thread,
      added_at_ms,
      ..
    } = upload.purpose
    {
      return self.complete_draft_file(upload, maybe_thread, added_at_ms);
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

impl UploadPurpose {
  pub fn is_message(&self) -> bool {
    matches!(self, Self::Message)
  }
}

impl DiskDownload {
  pub fn attachment_id(&self) -> AttachmentId {
    self.download.attachment_id()
  }

  /// Receives the rest of the file, checks and decrypts it and writes it into the download
  /// directory.
  #[cfg(not(target_arch = "wasm32"))]
  pub async fn save(self) -> Result<String, CoreError> {
    let Self {
      download,
      maybe_directory,
      maybe_target,
    } = self;
    if let Some(path) = maybe_target {
      // The user picked this file in a save dialog, which asked before replacing one. Written in
      // place, not through a temporary file beside it: on macOS the save dialog grants access to
      // the picked file only, not to other files in a protected folder (Downloads, Documents).
      // Opened only once the whole blob is in and checked, so a failed transfer leaves the picked
      // file as it was.
      let verified = download.receive().await?;
      let file = std::fs::File::create(&path).map_err(CoreError::LocalFile)?;
      verified.write_into(file, &path)?;
      return Ok(path.to_string_lossy().into_owned());
    }
    let directory =
      maybe_directory.ok_or_else(|| CoreError::InvalidRequest("no download directory configured".into()))?;
    let file_name = download.reference().file_name.clone();
    let verified = download.receive().await?;
    std::fs::create_dir_all(&directory).map_err(CoreError::LocalFile)?;
    let (file, path) = create_unique(&directory, &file_name)?;
    verified.write_into(file, &path)?;
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

/// The transfer of a [`DiskDownload`]: natively ranged into a partial file, in the browser whole.
#[cfg(not(target_arch = "wasm32"))]
type DiskTransfer = disk_downloads::RangedDownload;
#[cfg(target_arch = "wasm32")]
type DiskTransfer = BlobDownload;

/// A file on its way to disk (or, in the browser, to its download folder), detached from the client
/// like a [`super::BlobDownload`].
pub struct DiskDownload {
  download: DiskTransfer,
  #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
  maybe_directory: Option<std::path::PathBuf>,
  /// The exact file to write (picked in a save dialog) instead of a new name in the directory.
  #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
  maybe_target: Option<std::path::PathBuf>,
}

impl DiskDownload {
  fn new(
    download: DiskTransfer,
    target: &DownloadTarget,
    maybe_download_dir: Option<std::path::PathBuf>,
  ) -> Self {
    let (maybe_directory, maybe_target) = match target {
      DownloadTarget::DownloadDirectory => (maybe_download_dir, None),
      DownloadTarget::File { path } => (maybe_download_dir, Some(std::path::PathBuf::from(path))),
      DownloadTarget::Folder { path } => (Some(std::path::PathBuf::from(path)), None),
    };
    Self {
      download,
      maybe_directory,
      maybe_target,
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
pub(super) type BlobRequest = Pin<Box<dyn Future<Output = Result<Option<String>, ApiError>>>>;

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
    // A draft's file that is up but not kept stalls the same way (`keep_held_files`).
    let is_waiting = file.maybe_done.is_none() || file.is_held;
    if is_waiting && file.maybe_failure.is_none() && file.retry.is_stalled() {
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
  save_or_drop_upload(store, &upload)
}

/// Stops all files of an own message that is still going up (the one control of its upload): the
/// unfinished ones are given up on the server and the finished ones left out, so the message goes
/// out with its text only, or not at all when it has none. `false` when nothing of it is pending
/// any more (it went out meanwhile).
pub fn cancel_pending_upload(
  store: &impl LocalStore,
  conversation: ConversationId,
  message: ClientMessageId,
) -> Result<bool, CoreError> {
  let Some(mut upload) = pending_upload(store, conversation, message)? else {
    return Ok(false);
  };
  for file in upload.files.drain(..) {
    if file.maybe_done.is_none()
      && let Some(attachment) = announced_attachment(&file)
    {
      queue_abandon(store, attachment)?;
    }
  }
  save_or_drop_upload(store, &upload)
}

/// Keeps a pending upload that lost files while something of its message is left; otherwise drops
/// the message, which nobody else has seen.
fn save_or_drop_upload(store: &impl LocalStore, upload: &PendingUpload) -> Result<bool, CoreError> {
  let (conversation, message) = (upload.conversation_id, upload.client_message_id);
  let has_text = store.message(conversation, message)?.is_some_and(
    |stored| matches!(&stored.content, MessageContent::Text { body, .. } if !body.trim().is_empty()),
  );
  if !upload.files.is_empty() || has_text {
    // Completed by the host's next upload step when the rest is up.
    save_pending_upload(store, upload)?;
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
pub(super) fn announced_attachment(file: &PendingFile) -> Option<AttachmentId> {
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

/// Forgets the downloads whose files were deleted (`paths`, by the app's trimming of its download
/// folder), so they show as remote at once and are fetched again when opened, instead of pointing a
/// viewer at a file that is gone. Returns how many it forgot.
pub fn forget_deleted_downloads(
  store: &impl LocalStore,
  paths: &[std::path::PathBuf],
) -> Result<usize, CoreError> {
  if paths.is_empty() {
    return Ok(0);
  }
  let mut forgotten = 0;
  for (key, bytes) in store.list(Table::FileTransfers)? {
    // Other records live in this table too (failed uploads, abandoned files): not a state, skipped.
    let Ok(TransferState::Local { path }) = serde_json::from_slice::<TransferState>(&bytes) else {
      continue;
    };
    if paths.iter().any(|deleted| deleted == std::path::Path::new(&path)) {
      store.delete(Table::FileTransfers, &key)?;
      forgotten += 1;
    }
  }
  Ok(forgotten)
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
/// Whether every file is up or failed. A message's file held for a draft is up only once it is kept.
fn is_finished(upload: &PendingUpload) -> bool {
  let is_message = upload.purpose.is_message();
  upload
    .files
    .iter()
    .all(|file| (file.maybe_done.is_some() && !(is_message && file.is_held)) || file.maybe_failure.is_some())
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

/// A file read, encrypted and measured for its upload, with the thumbnail made from it.
struct SealedFile {
  content_len: u64,
  maybe_made: Option<Vec<u8>>,
  maybe_dimensions: Option<ImageDimensions>,
  encrypted: EncryptedAttachment,
}

/// Reads, thumbnails and encrypts `file`. Natively on a blocking thread (#1579): this is seconds of
/// work for a big photo on a phone, and on the engine's thread it held up every other job (the app's
/// sends and reads included) while a share's files went up one after another.
#[cfg(not(target_arch = "wasm32"))]
async fn seal_file(file: &PendingFile, makes_thumbnails: bool) -> Result<SealedFile, ()> {
  let source = file.source.clone();
  let (size_bytes, media_type) = (file.size_bytes, file.media_type.clone());
  // Only a browser's picked file is read asynchronously; it never gets here natively.
  let maybe_read = match &source {
    UploadSource::Picked { .. } => Some(read_source(&source, size_bytes).await?),
    UploadSource::Bytes { .. } | UploadSource::Path { .. } => None,
  };
  tokio::task::spawn_blocking(move || {
    let content = match maybe_read {
      Some(content) => content,
      None => read_source_now(&source)?,
    };
    let maybe_made = if makes_thumbnails && crate::thumbnail::is_image(&media_type) {
      crate::thumbnail::thumbnail(&content, &media_type)
    } else {
      None
    };
    seal_content(content, maybe_made, &media_type)
  })
  .await
  .map_err(|error| tracing::warn!(%error, "the file's encryption stopped"))?
}

/// [`seal_file`] in the browser: the page's codecs are asynchronous and there is one thread.
#[cfg(target_arch = "wasm32")]
async fn seal_file(file: &PendingFile, makes_thumbnails: bool) -> Result<SealedFile, ()> {
  let content = read_source(&file.source, file.size_bytes).await?;
  let maybe_made = if makes_thumbnails {
    crate::thumbnail::make_thumbnail(&content, &file.media_type).await
  } else {
    None
  };
  seal_content(content, maybe_made, &file.media_type)
}

fn seal_content(content: Vec<u8>, maybe_made: Option<Vec<u8>>, media_type: &str) -> Result<SealedFile, ()> {
  // Receivers reserve the picture's space before anything loads (issue #316).
  let maybe_dimensions = crate::thumbnail::image_dimensions(&content, media_type)
    .map(|(width, height)| ImageDimensions { width, height });
  let encrypted = encrypt_attachment(&content, DEFAULT_CHUNK_SIZE, &mut OsRng).map_err(|_| ())?;
  Ok(SealedFile {
    content_len: content.len() as u64,
    maybe_made,
    maybe_dimensions,
    encrypted,
  })
}

/// A waiting picture's preview (#1585): its upright size and, where the core can make one, its
/// thumbnail.
struct PicturePreview {
  dimensions: ImageDimensions,
  maybe_thumbnail: Option<Vec<u8>>,
}

/// Reads `file` and makes its [`PicturePreview`]; `None` when it can't be read or isn't a picture
/// the core reads. Natively on a blocking thread, like [`seal_file`]: decoding a big photo takes
/// seconds on a phone.
#[cfg(not(target_arch = "wasm32"))]
async fn preview_picture(file: &PendingFile) -> Option<PicturePreview> {
  let source = file.source.clone();
  let media_type = file.media_type.clone();
  let maybe_read = match &source {
    UploadSource::Picked { .. } => Some(read_source(&source, file.size_bytes).await.ok()?),
    UploadSource::Bytes { .. } | UploadSource::Path { .. } => None,
  };
  tokio::task::spawn_blocking(move || {
    let content = match maybe_read {
      Some(content) => content,
      None => read_source_now(&source).ok()?,
    };
    let (width, height) = crate::thumbnail::image_dimensions(&content, &media_type)?;
    Some(PicturePreview {
      dimensions: ImageDimensions { width, height },
      maybe_thumbnail: crate::thumbnail::thumbnail(&content, &media_type),
    })
  })
  .await
  .map_err(|error| tracing::warn!(%error, "the picture's preview stopped"))
  .ok()
  .flatten()
}

/// [`preview_picture`] in the browser: the page's codecs are asynchronous and there is one thread.
#[cfg(target_arch = "wasm32")]
async fn preview_picture(file: &PendingFile) -> Option<PicturePreview> {
  let content = read_source(&file.source, file.size_bytes).await.ok()?;
  let (width, height) = crate::thumbnail::image_dimensions(&content, &file.media_type)?;
  Some(PicturePreview {
    dimensions: ImageDimensions { width, height },
    maybe_thumbnail: crate::thumbnail::make_thumbnail(&content, &file.media_type).await,
  })
}

/// A file's bytes, read on the calling (blocking) thread.
#[cfg(not(target_arch = "wasm32"))]
fn read_source_now(source: &UploadSource) -> Result<Vec<u8>, ()> {
  match source {
    UploadSource::Bytes { bytes } => Ok(bytes.as_slice().to_vec()),
    UploadSource::Path { path } => std::fs::read(path).map_err(|error| {
      tracing::warn!(%error, "could not read a file to upload");
    }),
    UploadSource::Picked { .. } => Err(()),
  }
}

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

/// What a failed download says about where it failed: the card names it instead of a bare
/// "Transfer failed" (issue #807).
pub(crate) fn failure_of(error: &CoreError) -> TransferFailure {
  match error {
    CoreError::Api(ApiError::BlobTooLarge { .. }) => TransferFailure::TooLarge,
    CoreError::Api(ApiError::Intercepted { .. }) => TransferFailure::Intercepted,
    CoreError::Api(api) if api.is_not_found() || is_status(api, 410) => TransferFailure::Missing,
    CoreError::Api(_) if RetryCause::of(error) == RetryCause::StorageUnavailable => {
      TransferFailure::StorageUnavailable
    },
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

/// Creates `name`, or `name (2)`, `name (3)`, ... if taken, in `directory`. The name is sanitized
/// first. Each name is claimed by creating the file exclusively, so several downloads of files with
/// the same name into one folder ("Download all" of six `image.jpg`, #1014) never write over each
/// other or a file that was there before.
#[cfg(not(target_arch = "wasm32"))]
fn create_unique(
  directory: &std::path::Path,
  file_name: &str,
) -> Result<(std::fs::File, std::path::PathBuf), CoreError> {
  let safe = sanitize_file_name(file_name);
  let (stem, extension) = match safe.rsplit_once('.') {
    Some((stem, extension)) => (stem.to_string(), format!(".{extension}")),
    None => (safe.clone(), String::new()),
  };
  let candidates =
    std::iter::once(safe.clone()).chain((2..).map(|number| format!("{stem} ({number}){extension}")));
  for name in candidates.take(MAX_NAME_ATTEMPTS) {
    let path = directory.join(name);
    match std::fs::File::create_new(&path) {
      Ok(file) => return Ok((file, path)),
      Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
      Err(error) => return Err(CoreError::LocalFile(error)),
    }
  }
  Err(CoreError::LocalFile(std::io::Error::new(
    std::io::ErrorKind::AlreadyExists,
    "every numbered name is taken",
  )))
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

  /// A failed download keeps where it broke, so the card can say it (#807).
  #[test]
  fn a_failed_download_keeps_where_it_broke() {
    let api = |error: ApiError| failure_of(&CoreError::Api(error));
    assert_eq!(
      api(ApiError::Network("connection reset".into())),
      TransferFailure::Network
    );
    assert_eq!(
      api(ApiError::status(500, ErrorCode::Internal, "boom")),
      TransferFailure::Network
    );
    assert_eq!(
      api(ApiError::status(503, ErrorCode::Unavailable, "storage down")),
      TransferFailure::StorageUnavailable
    );
    assert_eq!(
      api(ApiError::status(404, ErrorCode::AttachmentNotFound, "gone")),
      TransferFailure::Missing
    );
    assert_eq!(
      api(ApiError::Intercepted {
        kind: crate::api::Interception::UntrustedCertificate,
        detail: "unknown issuer".into(),
      }),
      TransferFailure::Intercepted
    );
    assert_eq!(
      failure_of(&CoreError::Corrupt(
        "attachment blob doesn't match its hash".into()
      )),
      TransferFailure::Corrupted
    );
    let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    let error = CoreError::LocalFile(denied);
    assert_eq!(failure_of(&error), TransferFailure::LocalFile);
    assert!(
      error
        .to_string()
        .starts_with("writing the file on this device failed")
    );
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
    let created = |name: &str| create_unique(&directory, name).unwrap().1;
    let escaped = created("../../etc/passwd");
    assert_eq!(escaped.parent(), Some(directory.as_path()));
    assert_eq!(created("...").file_name().unwrap(), "file");
    std::fs::write(directory.join("a.txt"), b"x").unwrap();
    assert_eq!(created("a.txt").file_name().unwrap(), "a (2).txt");
    // Several files of the same name into one folder each get their own (#1014).
    assert_eq!(created("a.txt").file_name().unwrap(), "a (3).txt");
    assert_eq!(created("b").file_name().unwrap(), "b");
    assert_eq!(created("b").file_name().unwrap(), "b (2)");
    assert_eq!(std::fs::read(directory.join("a.txt")).unwrap(), b"x");
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
  fn deleted_downloads_are_forgotten_and_everything_else_stays() {
    let store = crate::store::MemoryStore::new();
    use teamcraft_proto::{Id, Snowflake};
    let id = |raw: i64| AttachmentId::from_snowflake(Snowflake::new(raw).unwrap());
    let (gone, kept, coming) = (id(1), id(2), id(3));
    set_transfer(
      &store,
      gone,
      &TransferState::Local {
        path: "/downloads/old.png".into(),
      },
    )
    .unwrap();
    set_transfer(
      &store,
      kept,
      &TransferState::Local {
        path: "/downloads/new.png".into(),
      },
    )
    .unwrap();
    set_transfer(&store, coming, &TransferState::Downloading { total_bytes: 9 }).unwrap();
    records::put_json(&store, Table::FileTransfers, ABANDONED_KEY, &vec![kept]).unwrap();

    let deleted = [std::path::PathBuf::from("/downloads/old.png")];
    assert_eq!(forget_deleted_downloads(&store, &deleted).unwrap(), 1);

    assert_eq!(
      transfer_state(&store, gone).unwrap(),
      None,
      "shows as remote again"
    );
    assert_eq!(
      transfer_state(&store, kept).unwrap(),
      Some(TransferState::Local {
        path: "/downloads/new.png".into()
      })
    );
    assert!(transfer_state(&store, coming).unwrap().is_some());
    assert!(
      records::get_json::<Vec<AttachmentId>>(&store, Table::FileTransfers, ABANDONED_KEY)
        .unwrap()
        .is_some(),
      "records of another kind stay"
    );
    assert_eq!(forget_deleted_downloads(&store, &[]).unwrap(), 0);
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
        maybe_waveform: None,
        is_held: false,
      }],
      purpose: UploadPurpose::Message,
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

    fn no_jitter() -> UploadRetrySettings {
      UploadRetrySettings {
        jitter: JitterMode::None,
        ..UploadRetrySettings::default()
      }
    }

    #[test]
    fn jitter_spreads_the_wait_between_half_and_all_of_the_backoff() {
      let settings = UploadRetrySettings::default();
      let waits: std::collections::HashSet<i64> = (0..40)
        .map(|_| {
          let mut retry = UploadRetry::default();
          retry.record_failure(0, &settings, RetryCause::StorageUnavailable);
          match retry {
            UploadRetry::Waiting { retry_at_ms, .. } => retry_at_ms,
            other => panic!("still retrying: {other:?}"),
          }
        })
        .collect();
      assert!(
        waits.iter().all(|wait| (1_000..=2_000).contains(wait)),
        "first wait of 2 s, half to all of it: {waits:?}"
      );
      assert!(waits.len() > 1, "devices don't all come back at the same moment");
    }

    #[test]
    fn waits_double_up_to_the_ceiling_and_stop_at_the_cap() {
      let settings = UploadRetrySettings {
        retry_after: std::time::Duration::from_secs(2),
        retry_max: std::time::Duration::from_secs(10),
        max_attempts: 5,
        stalled_retry_every: std::time::Duration::from_secs(300),
        jitter: JitterMode::None,
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
          cause: RetryCause::Network,
          maybe_retry_at_ms: Some(301_000),
        }
      );
      assert!(!retry.is_due(300_999), "a stalled file is left alone for a while");
      assert!(retry.is_due(301_000), "and then tried again by itself");
    }

    #[test]
    fn the_defaults_give_up_after_about_a_minute_never_endlessly() {
      let settings = no_jitter();
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
      let settings = no_jitter();
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
    fn a_stalled_file_keeps_being_tried_and_stays_stalled_while_it_fails() {
      let settings = UploadRetrySettings {
        max_attempts: 1,
        ..no_jitter()
      };
      let mut retry = UploadRetry::default();
      retry.record_failure(0, &settings, RetryCause::StorageUnavailable);
      assert!(retry.is_stalled());
      retry.record_failure(300_000, &settings, RetryCause::StorageUnavailable);
      assert_eq!(
        retry,
        UploadRetry::Stalled {
          attempts: 2,
          cause: RetryCause::StorageUnavailable,
          maybe_retry_at_ms: Some(600_000),
        },
        "the state does not change under the user while it is tried"
      );
    }

    #[test]
    fn a_stalled_file_waits_longer_when_the_server_asks_but_at_most_the_ceiling() {
      let settings = UploadRetrySettings {
        max_attempts: 1,
        ..no_jitter()
      };
      let retry_at = |server_wait| {
        let mut retry = UploadRetry::default();
        retry.record_failure_after(0, &settings, RetryCause::StorageUnavailable, Some(server_wait));
        match retry {
          UploadRetry::Stalled {
            maybe_retry_at_ms, ..
          } => maybe_retry_at_ms,
          other => panic!("stalled: {other:?}"),
        }
      };
      assert_eq!(retry_at(std::time::Duration::from_secs(30)), Some(300_000));
      assert_eq!(
        retry_at(std::time::Duration::from_secs(3_600)),
        Some(300_000),
        "a hostile server can't park the file"
      );
    }

    #[test]
    fn gateway_errors_in_front_of_the_storage_count_as_storage_outages() {
      let status = |status| {
        CoreError::Api(ApiError::Status {
          status,
          maybe_body: None,
          maybe_retry_after: None,
        })
      };
      for outage in [502, 503, 504] {
        assert_eq!(
          RetryCause::of(&status(outage)),
          RetryCause::StorageUnavailable,
          "{outage}"
        );
      }
      for other in [500, 403, 409] {
        assert_eq!(RetryCause::of(&status(other)), RetryCause::Network, "{other}");
      }
    }

    #[test]
    fn stored_slots_without_an_expiry_are_used_as_before() {
      let older: UploadSlot = serde_json::from_value(serde_json::json!({
        "attachment_id": "1",
        "upload_url": "https://storage.example.com/a",
        "size_bytes": 1,
      }))
      .expect("an older slot parses");
      assert!(!older.has_expired(i64::MAX));
      let slot = UploadSlot {
        maybe_expires_at_ms: Some(1_000),
        ..older
      };
      assert!(!slot.has_expired(999));
      assert!(slot.has_expired(1_000));
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
      assert!(
        older.is_due(0),
        "a file that stopped before this version tries again"
      );
      let unavailable = UploadRetry::Stalled {
        attempts: 6,
        cause: RetryCause::StorageUnavailable,
        maybe_retry_at_ms: None,
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
        maybe_waveform: None,
        is_held: false,
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
