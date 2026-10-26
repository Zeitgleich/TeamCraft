//! Files in the UI adapter: attachments of messages, uploads, downloads and share links.

use teamcraft_client_api::actions::{
  FileFailure, FileInfo, FileSource, FileTransfer, MediaAnchor, MediaPage, OutgoingFile, PixelSize,
  ShareLink, ShareLinkOptions,
};
use teamcraft_client_api::model::{ConversationId, MessageId, ThreadId};
use teamcraft_proto::{AttachmentId, Base64Bytes};

use crate::client::files::{
  PendingFile, PendingUpload, RetryCause, TransferFailure, TransferState, UploadRetry, UploadSource,
  attachments_of, cancel_pending_file, failed_uploads, pending_upload, save_pending_upload, share_links,
  transfer_state,
};
use crate::error::CoreError;
use crate::records;
use crate::store::{
  AttachmentRef, ImageDimensions, LocalStore, MediaRange, MessageContent, MessageMeta, StoredMessage, Table,
};
use crate::transfers::Transfers;
use crate::ui_backend::{CoreChatBackend, CoreCommand, RedactedText, ui_message_id};

impl<S: LocalStore> CoreChatBackend<S> {
  /// Files attached to a message, with where their bytes are.
  pub(crate) fn files_of(&self, stored: &StoredMessage) -> Vec<FileInfo> {
    let message = ui_message_id(stored);
    let conversation = stored.conversation_id;
    let (maybe_sender, maybe_sent_at) = (Some(stored.sender_user_id), Some(stored.sent_at_ms));
    if let Some(upload) = self.read(pending_upload(
      self.store_ref(),
      conversation,
      stored.client_message_id,
    )) {
      return upload
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| FileInfo {
          conversation,
          message,
          file_id: format!("pending-{index}"),
          file_name: file.file_name.clone(),
          media_type: file.media_type.clone(),
          size_bytes: file.size_bytes,
          maybe_thumbnail_png: file
            .maybe_thumbnail
            .as_ref()
            .map(|thumbnail| thumbnail.as_slice().to_vec()),
          transfer: upload_transfer(file, self.transfers()),
          maybe_sender,
          maybe_sent_at,
          maybe_dimensions: file.maybe_dimensions.map(|size| PixelSize {
            width: size.width,
            height: size.height,
          }),
          maybe_duration_ms: file.maybe_duration_ms,
        })
        .collect();
    }
    let failed = self
      .read(failed_uploads(
        self.store_ref(),
        conversation,
        stored.client_message_id,
      ))
      .into_iter()
      .enumerate()
      .map(|(index, file)| FileInfo {
        conversation,
        message,
        file_id: format!("failed-{index}"),
        file_name: file.file_name,
        media_type: file.media_type,
        size_bytes: file.size_bytes,
        maybe_thumbnail_png: None,
        transfer: FileTransfer::UploadFailed {
          reason: failure_view(file.maybe_failure.unwrap_or(TransferFailure::Network)),
        },
        maybe_sender,
        maybe_sent_at,
        maybe_dimensions: None,
        maybe_duration_ms: file.maybe_duration_ms,
      });
    attachments_of(&stored.content)
      .into_iter()
      .map(|attachment| FileInfo {
        conversation,
        message,
        file_id: attachment.attachment_id.to_string(),
        file_name: attachment.file_name.clone(),
        media_type: attachment.media_type.clone(),
        size_bytes: attachment.size_bytes,
        maybe_thumbnail_png: attachment
          .maybe_thumbnail
          .as_ref()
          .map(|png| png.as_slice().to_vec()),
        transfer: match self.read(transfer_state(self.store_ref(), attachment.attachment_id)) {
          // Opened (the picture, the viewer) or saved right now: how far it got.
          _ if self.transfers().progress(attachment.attachment_id).is_some() => {
            let maybe_progress = self.transfers().progress(attachment.attachment_id);
            FileTransfer::Downloading {
              done_bytes: maybe_progress.map_or(0, |progress| progress.of_size(attachment.size_bytes)),
              total_bytes: attachment.size_bytes,
            }
          },
          None => FileTransfer::Remote,
          Some(TransferState::Downloading { total_bytes }) => FileTransfer::Downloading {
            done_bytes: 0,
            total_bytes,
          },
          // A download whose file is gone (wiped at sign-out, cleared by the system) is remote
          // again.
          Some(TransferState::Local { path }) if std::path::Path::new(&path).is_file() => {
            FileTransfer::Local { path }
          },
          Some(TransferState::Local { .. }) => FileTransfer::Remote,
          Some(TransferState::Failed { reason }) => FileTransfer::Failed {
            reason: failure_view(reason),
          },
        },
        maybe_sender,
        maybe_sent_at,
        maybe_dimensions: attachment.maybe_dimensions.map(|size| PixelSize {
          width: size.width,
          height: size.height,
        }),
        maybe_duration_ms: attachment.maybe_duration_ms,
      })
      .chain(failed)
      .collect()
  }

  /// Stores the message right away and leaves the files to the engine, which uploads them and
  /// then sends the message.
  pub(crate) fn ui_send_files(
    &self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    body: &str,
    meta: MessageMeta,
    files: Vec<OutgoingFile>,
  ) {
    let mentions = self.mentions_in(conversation, body);
    let content = MessageContent::Text {
      body: body.to_string(),
      mentions,
      meta,
    };
    let Some(id) = self.enqueue(conversation, maybe_thread, content) else {
      return;
    };
    let upload = PendingUpload {
      conversation_id: conversation,
      client_message_id: id,
      files: files.into_iter().map(pending_file).collect(),
    };
    if let Err(error) = save_pending_upload(self.store_ref(), &upload) {
      tracing::error!(%error, "could not store the files to upload");
      return;
    }
    self.queue_link_preview(conversation, id, body);
    self.command(CoreCommand::UploadFiles);
  }

  /// "Cancel" on a file still going up; the host drops its running upload and sends the rest.
  pub(crate) fn ui_cancel_upload(&self, conversation: ConversationId, message: MessageId, file_id: &str) {
    let Some(index) = file_id
      .strip_prefix("pending-")
      .and_then(|index| index.parse::<usize>().ok())
    else {
      return;
    };
    let Some(stored) = self.find_stored(conversation, message) else {
      return;
    };
    match cancel_pending_file(self.store_ref(), conversation, stored.client_message_id, index) {
      Ok(true) => self.command(CoreCommand::UploadFiles),
      Ok(false) => {},
      Err(error) => tracing::error!(%error, "could not cancel the upload"),
    }
  }

  pub(crate) fn ui_conversation_files(&self, conversation: ConversationId) -> Vec<FileInfo> {
    let mut messages = self.all_messages(conversation);
    messages.reverse();
    messages.iter().flat_map(|stored| self.files_of(stored)).collect()
  }

  /// A page of at most `max_messages` messages with pictures or videos at `anchor`, newest first
  /// (the image viewer pages through them, #274). The store reads only the page: the viewer must
  /// neither rebuild the thumbnails of a whole conversation nor read all of its messages.
  pub(crate) fn ui_conversation_media(
    &self,
    conversation: ConversationId,
    anchor: MediaAnchor,
    max_messages: usize,
  ) -> MediaPage {
    if max_messages == 0 {
      return MediaPage::default();
    }
    // One more than asked for tells whether there are more beyond.
    let older = |from: Option<MessageId>, count: usize| {
      self.media_messages(conversation, MediaRange::Older(from), count.saturating_add(1))
    };
    let newer = |from: MessageId, count: usize| {
      self.media_messages(conversation, MediaRange::Newer(from), count.saturating_add(1))
    };
    let maybe_anchor_message = match anchor {
      MediaAnchor::Around(around) => self.media_message(conversation, around),
      _ => None,
    };
    let (newest_first, has_older, has_newer) = match (anchor, maybe_anchor_message) {
      (MediaAnchor::Around(around), Some(anchor_message)) => {
        // The message itself counts on the older side.
        let newer_count = max_messages / 2;
        let older_count = max_messages.saturating_sub(newer_count + 1);
        let mut newer_page = newer(around, newer_count);
        let has_newer = newer_page.len() > newer_count;
        newer_page.truncate(newer_count);
        newer_page.reverse();
        let mut older_page = older(Some(around), older_count);
        let has_older = older_page.len() > older_count;
        older_page.truncate(older_count);
        newer_page.push(anchor_message);
        newer_page.extend(older_page);
        (newer_page, has_older, has_newer)
      },
      (MediaAnchor::Older(from), _) => {
        let mut page = older(Some(from), max_messages);
        let has_older = page.len() > max_messages;
        page.truncate(max_messages);
        (page, has_older, true)
      },
      (MediaAnchor::Newer(from), _) => {
        let mut page = newer(from, max_messages);
        let has_newer = page.len() > max_messages;
        page.truncate(max_messages);
        page.reverse();
        (page, true, has_newer)
      },
      // Also a message that is gone: the newest page.
      (MediaAnchor::Newest | MediaAnchor::Around(_), _) => {
        let mut page = older(None, max_messages);
        let has_older = page.len() > max_messages;
        page.truncate(max_messages);
        (page, has_older, false)
      },
    };
    MediaPage {
      files: newest_first
        .iter()
        .flat_map(|stored| self.files_of(stored))
        .collect(),
      has_older,
      has_newer,
    }
  }

  fn media_messages(
    &self,
    conversation: ConversationId,
    range: MediaRange,
    limit: usize,
  ) -> Vec<StoredMessage> {
    self.read(
      self
        .store_ref()
        .media_messages(conversation, range, limit)
        .map_err(CoreError::from),
    )
  }

  /// The message with this server ID, when it has pictures or videos (one indexed read).
  fn media_message(&self, conversation: ConversationId, message: MessageId) -> Option<StoredMessage> {
    self
      .read(
        self
          .store_ref()
          .message_by_server_id(conversation, message)
          .map_err(CoreError::from),
      )
      .filter(StoredMessage::has_media)
  }

  pub(crate) fn ui_download_file(&self, conversation: ConversationId, message: MessageId) {
    let Some(stored) = self.find_stored(conversation, message) else {
      return;
    };
    for attachment in attachments_of(&stored.content) {
      self.command(CoreCommand::DownloadFile {
        conversation,
        attachment: attachment.attachment_id,
      });
    }
  }

  pub(crate) fn ui_download_attachment(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
  ) {
    match self.attachment_ref(conversation, message, file_id) {
      Some(attachment) => self.command(CoreCommand::DownloadFile {
        conversation,
        attachment: attachment.attachment_id,
      }),
      None => tracing::warn!(conversation_id = %conversation, "downloading a file that isn't loaded"),
    }
  }

  /// One file of a message, with the key to decrypt it ([`crate::Client::open_attachment`]).
  /// `file_id` is the [`FileInfo::file_id`] the UI got.
  pub fn attachment_ref(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
  ) -> Option<AttachmentRef> {
    let stored = self.find_stored(conversation, message)?;
    attachments_of(&stored.content)
      .into_iter()
      .find(|attachment| attachment.attachment_id.to_string() == file_id)
      .cloned()
  }

  pub(crate) fn ui_create_share_link(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
    options: ShareLinkOptions,
  ) {
    let Some(attachment) = self.attachment_id(conversation, message, file_id) else {
      tracing::warn!(conversation_id = %conversation, "sharing a file that isn't loaded");
      return;
    };
    self.command(CoreCommand::CreateShareLink {
      conversation,
      attachment,
      maybe_expires_in_seconds: options.maybe_expires_in_seconds,
      maybe_max_downloads: options.maybe_max_downloads,
      maybe_password: options.maybe_password.map(RedactedText),
    });
  }

  pub(crate) fn ui_revoke_share_link(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
    share_id: &str,
  ) {
    let Some(attachment) = self.attachment_id(conversation, message, file_id) else {
      return;
    };
    self.command(CoreCommand::RevokeShareLink {
      conversation,
      attachment,
      share_id: share_id.to_owned(),
    });
  }

  pub(crate) fn ui_refresh_share_links(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
  ) {
    let Some(attachment) = self.attachment_id(conversation, message, file_id) else {
      return;
    };
    self.command(CoreCommand::RefreshShareLinks { attachment });
  }

  pub(crate) fn ui_share_links(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
  ) -> Vec<ShareLink> {
    let Some(attachment) = self.attachment_id(conversation, message, file_id) else {
      return Vec::new();
    };
    self
      .read(share_links(self.store_ref(), attachment))
      .into_iter()
      .map(|link| ShareLink {
        file_id: link.attachment_id.to_string(),
        share_id: link.share_id,
        download_count: link.download_count,
        url: link.url,
        maybe_expires_at: link.maybe_expires_at,
        maybe_max_downloads: link.maybe_max_downloads,
        has_password: link.has_password,
      })
      .collect()
  }

  pub(crate) fn ui_max_upload_bytes(&self) -> Option<u64> {
    self.read(records::get_json(
      self.store_ref(),
      Table::Account,
      crate::client::MAX_UPLOAD_KEY,
    ))
  }

  /// The attachment ID of one file of a message; `None` if the message has no such file.
  fn attachment_id(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
  ) -> Option<AttachmentId> {
    let stored = self.find_stored(conversation, message)?;
    attachments_of(&stored.content)
      .into_iter()
      .map(|attachment| attachment.attachment_id)
      .find(|attachment| attachment.to_string() == file_id)
  }
}

fn pending_file(file: OutgoingFile) -> PendingFile {
  let (source, size_bytes) = match file.source {
    FileSource::Bytes { bytes } => {
      let size = bytes.len() as u64;
      (
        UploadSource::Bytes {
          bytes: Base64Bytes::new(bytes),
        },
        size,
      )
    },
    FileSource::Path { path } => {
      let size = file_size(&path);
      (UploadSource::Path { path }, size)
    },
    FileSource::Picked { handle, size_bytes } => (UploadSource::Picked { handle }, size_bytes),
  };
  // Read from the header only: the pending message reserves the picture's space at once.
  let maybe_dimensions = match &source {
    UploadSource::Bytes { bytes } => crate::thumbnail::image_dimensions(bytes.as_slice(), &file.media_type)
      .map(|(width, height)| ImageDimensions { width, height }),
    UploadSource::Path { .. } | UploadSource::Picked { .. } => None,
  };
  PendingFile {
    file_name: file.file_name,
    media_type: file.media_type,
    source,
    size_bytes,
    maybe_done: None,
    maybe_failure: None,
    maybe_slot: None,
    maybe_parts: None,
    retry: UploadRetry::default(),
    maybe_thumbnail: file.maybe_thumbnail.map(Base64Bytes::new),
    maybe_dimensions,
    maybe_duration_ms: file.maybe_duration_ms,
  }
}

#[cfg(not(target_arch = "wasm32"))]
fn file_size(path: &str) -> u64 {
  std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

#[cfg(target_arch = "wasm32")]
fn file_size(_path: &str) -> u64 {
  0
}

/// Progress of one file of an upload: the bytes sent while it goes up, all once it is up.
fn upload_transfer(file: &PendingFile, transfers: &Transfers) -> FileTransfer {
  if let Some(reason) = file.maybe_failure {
    return FileTransfer::UploadFailed {
      reason: failure_view(reason),
    };
  }
  if file.maybe_done.is_none()
    && let Some(cause) = file.retry.stall_cause()
  {
    // The stall says why (#742): an outage of the storage reads differently from a network failure.
    return FileTransfer::UploadStalled {
      reason: match cause {
        RetryCause::Network => FileFailure::Network,
        RetryCause::StorageUnavailable => FileFailure::StorageUnavailable,
      },
    };
  }
  let done_bytes = match (&file.maybe_done, &file.maybe_slot) {
    (Some(_), _) => file.size_bytes,
    (None, Some(slot)) => transfers
      .progress(slot.attachment_id)
      .map_or(0, |progress| progress.of_size(file.size_bytes)),
    (None, None) => 0,
  };
  FileTransfer::Uploading {
    done_bytes,
    total_bytes: file.size_bytes,
    failed_attempts: file.retry.failed_attempts(),
  }
}

fn failure_view(reason: TransferFailure) -> FileFailure {
  match reason {
    TransferFailure::TooLarge => FileFailure::TooLarge,
    TransferFailure::Network => FileFailure::Network,
    TransferFailure::Corrupted => FileFailure::Corrupted,
    TransferFailure::LocalFile => FileFailure::LocalFile,
  }
}
