//! Files uploaded at once, outside the outbox's pending uploads (docs/bridge.md, "Files"). The
//! integration bridge uploads the files of a webhook first and queues their message only once every
//! one is up, so a failure answers the producer's request and leaves no message with files missing.
//!
//! Each file goes up exactly like one of the outbox's (e2ee.md §4.5): encrypted with a fresh key,
//! a large file in parts through presigned part URLs and read one part at a time, a picture with
//! its thumbnail and size. Nothing is retried or kept in the store: the producer, which still waits
//! for its answer, retries.

use teamcraft_proto::{AttachmentId, ClientMessageId, ConversationId, UserId};

use super::Client;
use super::files::{
  BlobRequest, PendingFile, TransferFailure, UploadError, UploadRetry, UploadSource, UploadTarget,
  announced_attachment,
};
use super::upload_parts::PartOutcome;
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::store::{AttachmentRef, LocalStore, MessageContent, MessageMeta};
use crate::thumbnail::MAX_THUMBNAIL_BYTES;
use crate::transfers::TransferGuard;

/// Thumbnails one message carries at most, together: the message has to stay within the server's
/// ciphertext limit (1 MiB by default), and base64 makes each a third larger. Files beyond it go
/// without their preview.
pub const MAX_MESSAGE_THUMBNAIL_BYTES: usize = 4 * MAX_THUMBNAIL_BYTES;

/// A file on its way up, outside the outbox. Drive it with [`Client::start_direct_upload`] and
/// [`Client::finish_direct_upload`] until the latter returns the file's reference; give it up with
/// [`Client::abandon_direct_upload`] when it fails or is no longer wanted.
pub struct DirectUpload {
  conversation_id: ConversationId,
  file: PendingFile,
  /// Every attachment the server announced for the file, to give up if it fails.
  announced: Vec<AttachmentId>,
}

/// One step of a [`DirectUpload`] (the whole file, or one part of a large one), detached from the
/// client: the host drives [`Self::sent`] beside its loop, so a large file doesn't hold up the rest,
/// then hands the step to [`Client::finish_direct_upload`]. Dropping it stops the transfer.
pub struct DirectUploadStep {
  target: UploadTarget,
  /// `None` when nothing goes to the storage (the parts of a file are only joined).
  maybe_request: Option<BlobRequest>,
  /// What storage answered once [`Self::sent`] resolved successfully: a part's `ETag`.
  maybe_etag: Option<String>,
  /// Lists the transfer's progress while the step lives.
  _progress: TransferGuard,
}

/// Why a [`DirectUpload`] failed.
#[derive(Debug, thiserror::Error)]
pub enum DirectUploadError {
  /// Larger than this client's limit (`ClientSettings::max_upload_bytes`), the server's attachment
  /// limit or the account's attachment storage.
  #[error("the file is larger than the server takes")]
  TooLarge,
  /// The server or the storage could not be reached.
  #[error("the server can't be reached: {0}")]
  Offline(#[source] CoreError),
  /// The server or the storage refused or failed the upload.
  #[error("the upload failed: {0}")]
  Failed(#[source] CoreError),
  /// The file could not be read (or changed while it went up).
  #[error("the file could not be read")]
  LocalFile,
}

impl DirectUpload {
  /// A file of `size_bytes` from `source` (a path on this device is read one part at a time; it must
  /// not change until the upload is finished).
  pub fn new(
    conversation_id: ConversationId,
    source: UploadSource,
    file_name: String,
    media_type: String,
    size_bytes: u64,
  ) -> Self {
    Self {
      conversation_id,
      file: PendingFile {
        file_name,
        media_type,
        source,
        size_bytes,
        maybe_done: None,
        maybe_failure: None,
        maybe_slot: None,
        retry: UploadRetry::default(),
        maybe_thumbnail: None,
        maybe_dimensions: None,
        maybe_duration_ms: None,
        maybe_parts: None,
      },
      announced: Vec::new(),
    }
  }

  /// Remembers the attachment the server announced for the file, which a failure may drop from the
  /// file's state (a refused slot, a lost upload in parts).
  fn note_announced(&mut self) {
    if let Some(attachment) = announced_attachment(&self.file)
      && !self.announced.contains(&attachment)
    {
      self.announced.push(attachment);
    }
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Starts the upload's next step: encrypts the file (or its next part), announces it at the
  /// server first if needed and starts sending it. The bytes go in the returned step.
  pub async fn start_direct_upload(
    &mut self,
    upload: &mut DirectUpload,
  ) -> Result<DirectUploadStep, DirectUploadError> {
    let started = self.start_file(upload.conversation_id, &mut upload.file).await;
    upload.note_announced();
    let (target, maybe_request, progress) = started?;
    Ok(DirectUploadStep {
      target,
      maybe_request,
      maybe_etag: None,
      _progress: progress,
    })
  }

  /// Completes a step the host drove: confirms a whole file, or records a part and joins the parts
  /// after the last one. `Ok(Some(_))`: the file is up, with what its message needs; `Ok(None)`: a
  /// part is stored and the next step follows.
  pub async fn finish_direct_upload(
    &mut self,
    upload: &mut DirectUpload,
    step: DirectUploadStep,
    sent: Result<(), ApiError>,
  ) -> Result<Option<AttachmentRef>, DirectUploadError> {
    let DirectUploadStep {
      target, maybe_etag, ..
    } = step;
    let finished = match target {
      UploadTarget::Whole { slot, attachment } => self
        .confirm_blob(&slot, sent, &mut upload.file)
        .await
        .map(|()| PartOutcome::Uploaded(attachment)),
      UploadTarget::Part(target) => {
        // A stored part's ETag was kept by `DirectUploadStep::sent`.
        let answer = sent.map(|()| maybe_etag);
        self.finish_part(target, answer, &mut upload.file).await
      },
    };
    upload.note_announced();
    match finished? {
      PartOutcome::Uploaded(attachment) => Ok(Some(attachment)),
      PartOutcome::Stored => Ok(None),
    }
  }

  /// Gives the upload up at the server, which frees what it stored so far. A file that is up
  /// already stays: the server keeps completed attachments, but no message will carry its key.
  /// Best effort; the server sweeps unfinished uploads it doesn't hear about.
  pub async fn abandon_direct_upload(&self, upload: &DirectUpload) {
    for attachment in &upload.announced {
      if let Err(error) = self.api.abandon_attachment(*attachment).await {
        tracing::debug!(%error, "an upload was not given up");
      }
    }
  }

  /// Puts a text with files that are up already into the outbox; it goes out with the next flush.
  /// Thumbnails beyond [`MAX_MESSAGE_THUMBNAIL_BYTES`] are left out, so the message stays within the
  /// server's size limit.
  pub fn queue_text_with_files(
    &mut self,
    conversation: ConversationId,
    body: &str,
    mentions: Vec<UserId>,
    attachments: Vec<AttachmentRef>,
  ) -> Result<ClientMessageId, CoreError> {
    let content = MessageContent::Text {
      body: body.to_string(),
      mentions,
      meta: MessageMeta {
        attachments: within_thumbnail_budget(attachments),
        ..MessageMeta::default()
      },
    };
    self.enqueue(conversation, None, content)
  }
}

impl DirectUploadStep {
  /// Resolves once storage answered. Polling may stop half way (a host's `select`) and resume on
  /// the next call; after it resolved, the step goes to [`Client::finish_direct_upload`].
  pub async fn sent(&mut self) -> Result<(), ApiError> {
    let answer = match &mut self.maybe_request {
      Some(request) => request.await,
      None => Ok(None),
    };
    self.maybe_request = None;
    self.maybe_etag = answer?;
    Ok(())
  }
}

impl From<UploadError> for DirectUploadError {
  fn from(error: UploadError) -> Self {
    match error {
      UploadError::Offline(error) => Self::Offline(error),
      UploadError::Retry(error) => Self::Failed(error),
      UploadError::Failed(TransferFailure::TooLarge) => Self::TooLarge,
      UploadError::Failed(_) => Self::LocalFile,
    }
  }
}

/// Keeps the thumbnails of the first files up to [`MAX_MESSAGE_THUMBNAIL_BYTES`] together; the rest
/// go without.
fn within_thumbnail_budget(mut attachments: Vec<AttachmentRef>) -> Vec<AttachmentRef> {
  let mut used = 0;
  for attachment in &mut attachments {
    let Some(thumbnail) = &attachment.maybe_thumbnail else {
      continue;
    };
    let size = thumbnail.as_slice().len();
    if used + size > MAX_MESSAGE_THUMBNAIL_BYTES {
      attachment.maybe_thumbnail = None;
    } else {
      used += size;
    }
  }
  attachments
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Base64Bytes, Id, Snowflake};

  use super::*;

  #[test]
  fn thumbnails_beyond_the_message_budget_are_left_out() {
    let attachments: Vec<AttachmentRef> = (1..=6)
      .map(|id| with_thumbnail(id, MAX_THUMBNAIL_BYTES))
      .collect();
    let kept = within_thumbnail_budget(attachments);
    let with_preview = kept
      .iter()
      .filter(|attachment| attachment.maybe_thumbnail.is_some())
      .count();
    assert_eq!(with_preview, 4);
    assert!(kept[4].maybe_thumbnail.is_none() && kept[5].maybe_thumbnail.is_none());
    assert_eq!(kept.len(), 6, "every file stays");
  }

  #[test]
  fn a_small_thumbnail_after_one_that_did_not_fit_is_kept() {
    let mut attachments: Vec<AttachmentRef> = (1..=3)
      .map(|id| with_thumbnail(id, MAX_THUMBNAIL_BYTES))
      .collect();
    attachments.push(with_thumbnail(4, 2 * MAX_THUMBNAIL_BYTES));
    attachments.push(with_thumbnail(5, 10));
    let kept = within_thumbnail_budget(attachments);
    assert!(kept[3].maybe_thumbnail.is_none());
    assert!(kept[4].maybe_thumbnail.is_some());
  }

  fn with_thumbnail(id: i64, size: usize) -> AttachmentRef {
    AttachmentRef {
      attachment_id: AttachmentId::from_snowflake(Snowflake::new(id).unwrap()),
      key: Base64Bytes::new(vec![0; 32]),
      ciphertext_sha256: Base64Bytes::new(vec![0; 32]),
      file_name: format!("{id}.png"),
      media_type: "image/png".into(),
      size_bytes: 1,
      maybe_thumbnail: Some(Base64Bytes::new(vec![0; size])),
      maybe_dimensions: None,
      maybe_duration_ms: None,
    }
  }
}
