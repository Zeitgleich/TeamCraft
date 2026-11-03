//! Files uploaded at once, outside the outbox's pending uploads (docs/bridge.md, "Files"). The
//! integration bridge uploads the files of a webhook first and queues their message only once every
//! one is up, so a failure answers the producer's request and leaves no message with files missing.
//!
//! Each file goes up exactly like one of the outbox's (e2ee.md §4.5): encrypted with a fresh key,
//! a large file in parts through presigned part URLs and read one part at a time, a picture with
//! its thumbnail and size. Nothing is retried or kept in the store: the producer, which still waits
//! for its answer, retries.
//!
//! Sending a file's bytes and completing its upload are separate steps, so a host completes the
//! files of a message only once all of them are up. Until then each is an unfinished upload that
//! the server frees when it is given up; a failure half way leaves nothing behind.

use teamcraft_proto::{AttachmentId, Base64Bytes, ClientMessageId, ConversationId, Id, Snowflake, UserId};

use super::Client;
use super::files::{
  BlobRequest, PendingFile, TransferFailure, UploadError, UploadRetry, UploadSlot, UploadSource,
  UploadTarget, announced_attachment,
};
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::store::{AttachmentRef, ImageDimensions, LocalStore, MessageContent, MessageMeta};
use crate::thumbnail::MAX_THUMBNAIL_BYTES;
use crate::transfers::TransferGuard;

/// Thumbnails one message carries at most, together: the message has to stay within the server's
/// ciphertext limit (1 MiB by default), and base64 makes each a third larger. Files beyond it go
/// without their preview.
pub const MAX_MESSAGE_THUMBNAIL_BYTES: usize = 4 * MAX_THUMBNAIL_BYTES;
/// What a message adds to its content on the way, generously: the sender's workspace head, the
/// encryption's framing and authentication.
const MESSAGE_OVERHEAD_BYTES: usize = 16 * 1024;

/// A file on its way up, outside the outbox. Drive it with [`Client::start_direct_upload`] and
/// [`Client::finish_direct_upload`] until every byte is sent, then [`Client::complete_direct_upload`]
/// gives its reference; give it up with [`Client::abandon_direct_upload`] when it fails or is no
/// longer wanted.
pub struct DirectUpload {
  conversation_id: ConversationId,
  file: PendingFile,
  /// Every attachment the server announced for the file, to give up if it fails.
  announced: Vec<AttachmentId>,
  /// Set once every byte is up: what completing the upload needs.
  maybe_sent: Option<SentFile>,
}

/// Where a [`DirectUpload`] stands after a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectUploadProgress {
  /// A part is stored; the next step follows.
  MoreToSend,
  /// Every byte is up; [`Client::complete_direct_upload`] finishes the upload.
  AllSent,
}

/// A file whose bytes are all up, before its upload is completed.
enum SentFile {
  /// In one blob, in its announced slot.
  Whole {
    slot: UploadSlot,
    attachment: Box<AttachmentRef>,
  },
  /// In parts; their `ETag`s are on the file.
  Parts,
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
      maybe_sent: None,
    }
  }

  /// A picture's thumbnail and size, made by the host ([`crate::ClientSettings::makes_thumbnails`]
  /// off): they go into the file's reference as they are (a thumbnail larger than a message allows
  /// is dropped).
  pub fn with_picture(
    mut self,
    maybe_thumbnail: Option<Vec<u8>>,
    maybe_dimensions: Option<(u32, u32)>,
  ) -> Self {
    self.file.maybe_thumbnail = maybe_thumbnail.map(Base64Bytes::new);
    self.file.maybe_dimensions = maybe_dimensions.map(|(width, height)| ImageDimensions { width, height });
    self
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

  /// Records a step the host drove: a whole file is sent, or a part is stored. Nothing is completed
  /// yet ([`Self::complete_direct_upload`]), so the upload can still be given up whole.
  pub async fn finish_direct_upload(
    &mut self,
    upload: &mut DirectUpload,
    step: DirectUploadStep,
    sent: Result<(), ApiError>,
  ) -> Result<DirectUploadProgress, DirectUploadError> {
    let DirectUploadStep {
      target, maybe_etag, ..
    } = step;
    let recorded = match target {
      UploadTarget::Whole { slot, attachment } => match sent {
        Ok(()) => {
          upload.maybe_sent = Some(SentFile::Whole {
            slot,
            attachment: Box::new(attachment),
          });
          Ok(DirectUploadProgress::AllSent)
        },
        Err(error) if error.is_unreachable() => Err(DirectUploadError::Offline(CoreError::Api(error))),
        Err(error) => Err(DirectUploadError::Failed(CoreError::Api(error))),
      },
      UploadTarget::Part(target) => {
        // A stored part's ETag was kept by `DirectUploadStep::sent`.
        let answer = sent.map(|()| maybe_etag);
        match self.record_part(target, answer, &mut upload.file).await {
          Ok(true) => {
            upload.maybe_sent = Some(SentFile::Parts);
            Ok(DirectUploadProgress::AllSent)
          },
          Ok(false) => Ok(DirectUploadProgress::MoreToSend),
          Err(error) => Err(error.into()),
        }
      },
    };
    upload.note_announced();
    recorded
  }

  /// Completes an upload whose bytes are all up (the server checks the stored size) and returns
  /// what its message needs. After this the server keeps the file: complete the files of a message
  /// only once all of them are up.
  pub async fn complete_direct_upload(
    &mut self,
    upload: &mut DirectUpload,
  ) -> Result<AttachmentRef, DirectUploadError> {
    let completed = match upload.maybe_sent.take() {
      Some(SentFile::Whole { slot, attachment }) => self
        .confirm_blob(&slot, Ok(()), &mut upload.file)
        .await
        .map(|()| *attachment),
      Some(SentFile::Parts) => self.join_parts(&mut upload.file).await,
      None => {
        return Err(DirectUploadError::Failed(CoreError::InvalidRequest(
          "the file is not up yet".into(),
        )));
      },
    };
    upload.note_announced();
    Ok(completed?)
  }

  /// Gives the upload up at the server, which frees what it stored so far. A file whose upload was
  /// completed stays: the server keeps completed attachments, but no message will carry its key.
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

/// About how large a message with `body`, `mention_count` mentions and files of these names and
/// media types gets once encrypted, at most: every picture counted with the largest thumbnail, up
/// to the message's thumbnail budget. A host checks it against the server's message limit before it
/// uploads anything.
pub fn estimated_message_bytes(body: &str, mention_count: usize, files: &[(&str, &str)]) -> usize {
  // The longest IDs: as many digits as there can be.
  let Ok(largest_id) = Snowflake::new(i64::MAX) else {
    return usize::MAX;
  };
  let placeholder = |(file_name, media_type): &(&str, &str)| AttachmentRef {
    attachment_id: AttachmentId::from_snowflake(largest_id),
    key: Base64Bytes::new(vec![u8::MAX; 32]),
    ciphertext_sha256: Base64Bytes::new(vec![u8::MAX; 32]),
    file_name: (*file_name).to_owned(),
    media_type: (*media_type).to_owned(),
    size_bytes: u64::MAX,
    // Room for the field itself; the bytes are counted below.
    maybe_thumbnail: Some(Base64Bytes::new(Vec::new())),
    maybe_dimensions: Some(ImageDimensions {
      width: u32::MAX,
      height: u32::MAX,
    }),
    maybe_duration_ms: None,
  };
  let content = MessageContent::Text {
    body: body.to_owned(),
    mentions: vec![UserId::from_snowflake(largest_id); mention_count],
    meta: MessageMeta {
      attachments: files.iter().map(placeholder).collect(),
      ..MessageMeta::default()
    },
  };
  // Serializing plain data can't fail; if it did, no size is small enough.
  let content_bytes = serde_json::to_vec(&content).map_or(usize::MAX, |json| json.len());
  let pictures = files
    .iter()
    .filter(|(_, media_type)| media_type.starts_with("image/"))
    .count();
  let thumbnail_bytes = pictures
    .saturating_mul(MAX_THUMBNAIL_BYTES)
    .min(MAX_MESSAGE_THUMBNAIL_BYTES)
    .div_ceil(3)
    * 4;
  content_bytes
    .saturating_add(thumbnail_bytes)
    .saturating_add(MESSAGE_OVERHEAD_BYTES)
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
