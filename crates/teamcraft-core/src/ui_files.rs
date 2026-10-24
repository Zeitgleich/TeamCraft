//! Files in the UI adapter: attachments of messages, uploads, downloads and share links.

use teamcraft_client_api::actions::{
  FileFailure, FileInfo, FileSource, FileTransfer, OutgoingFile, PixelSize, ShareLink, ShareLinkOptions,
};
use teamcraft_client_api::model::{ConversationId, MessageId, ThreadId};
use teamcraft_proto::{AttachmentId, Base64Bytes};

use crate::client::files::{
  PendingFile, PendingUpload, TransferFailure, TransferState, UploadRetry, UploadSource, attachments_of,
  failed_uploads, pending_upload, save_pending_upload, share_links, transfer_state,
};
use crate::records;
use crate::store::{AttachmentRef, LocalStore, MessageContent, MessageMeta, StoredMessage, Table};
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
          maybe_thumbnail_png: None,
          transfer: upload_transfer(&upload, file),
          maybe_sender,
          maybe_sent_at,
          maybe_dimensions: None,
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

  pub(crate) fn ui_conversation_files(&self, conversation: ConversationId) -> Vec<FileInfo> {
    let mut messages = self.all_messages(conversation);
    messages.reverse();
    messages.iter().flat_map(|stored| self.files_of(stored)).collect()
  }

  /// The files of a window of at most `max_messages` messages around `around`, newest first: the
  /// image viewer must not rebuild the thumbnails of a whole conversation. Only messages with
  /// attachments count towards the window.
  pub(crate) fn ui_conversation_images(
    &self,
    conversation: ConversationId,
    around: MessageId,
    max_messages: usize,
  ) -> Vec<FileInfo> {
    let mut messages: Vec<StoredMessage> = self
      .all_messages(conversation)
      .into_iter()
      .filter(|stored| !attachments_of(&stored.content).is_empty())
      .collect();
    messages.reverse();
    // An unknown message (deleted meanwhile) gets the newest window.
    let at = messages
      .iter()
      .position(|stored| ui_message_id(stored) == around)
      .unwrap_or(0);
    let start = at.saturating_sub(max_messages / 2);
    messages
      .iter()
      .skip(start)
      .take(max_messages)
      .flat_map(|stored| self.files_of(stored))
      .collect()
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
      maybe_expires_at: options.maybe_expires_at,
      maybe_max_downloads: options.maybe_max_downloads,
      maybe_password: options.maybe_password.map(RedactedText),
    });
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
  };
  PendingFile {
    file_name: file.file_name,
    media_type: file.media_type,
    source,
    size_bytes,
    maybe_done: None,
    maybe_failure: None,
    maybe_slot: None,
    retry: UploadRetry::default(),
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

/// Progress of a file in an upload: done files count fully, the rest not at all (uploads are one
/// request per file).
fn upload_transfer(upload: &PendingUpload, file: &PendingFile) -> FileTransfer {
  if let Some(reason) = file.maybe_failure {
    return FileTransfer::UploadFailed {
      reason: failure_view(reason),
    };
  }
  if file.maybe_done.is_none() && file.retry.is_stalled() {
    return FileTransfer::UploadStalled {
      reason: FileFailure::Network,
    };
  }
  let total_bytes: u64 = upload.files.iter().map(|file| file.size_bytes).sum();
  let done_bytes: u64 = upload
    .files
    .iter()
    .filter(|file| file.maybe_done.is_some())
    .map(|file| file.size_bytes)
    .sum();
  FileTransfer::Uploading {
    done_bytes,
    total_bytes,
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
