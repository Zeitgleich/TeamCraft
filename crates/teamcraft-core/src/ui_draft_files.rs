//! Files of unsent drafts in the UI adapter (#1220): attaching, listing and removing them, and
//! sending them with their message. See `client::draft_files` for how they travel.

use teamcraft_client_api::actions::OutgoingFile;
use teamcraft_client_api::drafts::{DraftFileItem, DraftFileState};
use teamcraft_client_api::model::{ConversationId, ThreadId};
use teamcraft_proto::ClientMessageId;

use crate::account_data::AccountData;
use crate::client::draft_files::{
  DraftFileSource, attach_draft_files, draft_file_retention_days, find_draft_file, is_draft_file_expired,
  own_draft_file, remove_pending_draft_file,
};
use crate::client::files::{PendingFile, PendingUpload, UploadPurpose, UploadRetry, UploadSource};
use crate::client::stored_account_data;
use crate::records;
use crate::store::{AttachmentRef, LocalStore, Table};
use crate::ui_backend::{CoreChatBackend, CoreCommand};
use crate::ui_files::pending_file;

impl<S: LocalStore> CoreChatBackend<S> {
  pub(crate) fn ui_attach_draft_files(
    &self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    files: Vec<OutgoingFile>,
  ) {
    let files: Vec<(Option<ClientMessageId>, PendingFile)> = files
      .into_iter()
      .filter_map(|file| {
        let maybe_id = file
          .maybe_draft_id
          .as_deref()
          .and_then(|id| ClientMessageId::from_hex(id).ok());
        Some((maybe_id, pending_file(file)?))
      })
      .collect();
    if files.is_empty() {
      return;
    }
    let data = self.read(stored_account_data(self.store_ref()));
    let attached = attach_draft_files(
      self.store_ref(),
      &data,
      conversation,
      maybe_thread,
      files,
      self.now_ms(),
    );
    if !self.read(attached).is_empty() {
      self.command(CoreCommand::UploadFiles);
    }
  }

  /// The draft's files, those still on this device and those in the account data, in the order
  /// they were attached. Reads only this conversation's pending uploads.
  pub(crate) fn ui_draft_files(
    &self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
  ) -> Vec<DraftFileItem> {
    let data = self.read(stored_account_data(self.store_ref()));
    let maybe_retention = draft_file_retention_days(self.store_ref());
    let can_travel = maybe_retention.is_some();
    let now = self.now_ms();
    let is_expired = |added_at_ms: i64| is_draft_file_expired(added_at_ms, maybe_retention, now);
    let prefix = format!("{conversation}/");
    let pending = self.read(records::list_json_with_prefix::<PendingUpload>(
      self.store_ref(),
      Table::PendingUploads,
      &prefix,
    ));
    let mut files: Vec<(i64, DraftFileItem)> = pending
      .iter()
      .filter_map(|(_, upload)| match upload.purpose {
        UploadPurpose::Draft {
          maybe_thread: thread,
          added_at_ms,
          ..
        } if thread == maybe_thread => {
          let mut item = pending_item(upload, can_travel)?;
          // Up and held, but kept on this device (the drafts were full): it expires the same way.
          let is_held_up = upload
            .files
            .iter()
            .any(|file| file.is_held && file.maybe_done.is_some());
          if is_held_up && is_expired(added_at_ms) {
            item.state = DraftFileState::Expired;
          }
          Some((added_at_ms, item))
        },
        _ => None,
      })
      .collect();
    files.extend(
      data
        .draft_files(conversation, maybe_thread)
        .into_iter()
        .map(|(id, file)| {
          // The uploading device's own copy keeps a thumbnail too large to travel.
          let own = own_draft_file(self.store_ref(), id);
          let mut item = uploaded_item(id, own.as_ref().unwrap_or(&file.attachment));
          if is_expired(file.added_at_ms) {
            item.state = DraftFileState::Expired;
          }
          (file.added_at_ms, item)
        }),
    );
    files.sort_by(|(a_time, a), (b_time, b)| (a_time, &a.id).cmp(&(b_time, &b.id)));
    files.into_iter().map(|(_, item)| item).collect()
  }

  pub(crate) fn ui_remove_draft_file(&self, conversation: ConversationId, id: &str) {
    if self.read(remove_pending_draft_file(self.store_ref(), conversation, id)) {
      self.command(CoreCommand::UploadFiles);
      return;
    }
    self.command(CoreCommand::RemoveDraftFile { id: id.to_owned() });
  }

  /// Takes a file of the draft for its message. A file still on this device leaves the draft's
  /// uploads and continues as the message's; an uploaded one comes from the account data (taken out
  /// of the draft on every device once the message is queued), unless the server has likely
  /// deleted it by now.
  pub(crate) fn take_draft_file(
    &self,
    data: &AccountData,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    id: &str,
  ) -> Option<TakenDraftFile> {
    let found =
      find_draft_file(self.store_ref(), data, conversation, maybe_thread, id).unwrap_or_else(|error| {
        tracing::error!(%error, "local store read failed");
        None
      });
    match found? {
      DraftFileSource::Pending(upload) => {
        let key = records::outbox_key(conversation, upload.client_message_id);
        if let Err(error) = self.store_ref().delete(Table::PendingUploads, &key) {
          tracing::error!(%error, "could not take the draft's file for its message");
          return None;
        }
        // Held if it was announced as a draft's file; the message keeps it once it is up.
        Some(TakenDraftFile::OnDevice(upload.files.into_iter().next()?))
      },
      DraftFileSource::Uploaded {
        attachment,
        added_at_ms,
      } => {
        let maybe_retention = draft_file_retention_days(self.store_ref());
        if is_draft_file_expired(added_at_ms, maybe_retention, self.now_ms()) {
          Some(TakenDraftFile::Expired)
        } else {
          Some(TakenDraftFile::Uploaded(uploaded_file(attachment)))
        }
      },
    }
  }
}

/// A draft's file as a message takes it.
pub(crate) enum TakenDraftFile {
  /// Still on this device: it goes up with the message.
  OnDevice(PendingFile),
  /// Up already, from the account data: kept as the message goes out.
  Uploaded(PendingFile),
  /// Up, but held so long the server has likely deleted it: not sent from the draft.
  Expired,
}

/// An uploaded draft's file as a message's file: up already, held until the message keeps it.
fn uploaded_file(attachment: AttachmentRef) -> PendingFile {
  PendingFile {
    file_name: attachment.file_name.clone(),
    media_type: attachment.media_type.clone(),
    source: UploadSource::Bytes {
      bytes: teamcraft_proto::Base64Bytes::new(Vec::new()),
    },
    size_bytes: attachment.size_bytes,
    maybe_failure: None,
    maybe_slot: None,
    maybe_parts: None,
    retry: UploadRetry::default(),
    maybe_thumbnail: attachment.maybe_thumbnail.clone(),
    maybe_dimensions: attachment.maybe_dimensions,
    maybe_duration_ms: attachment.maybe_duration_ms,
    maybe_waveform: attachment.maybe_waveform.clone(),
    maybe_done: Some(attachment),
    is_held: true,
  }
}

fn pending_item(upload: &PendingUpload, can_travel: bool) -> Option<DraftFileItem> {
  let file = upload.files.first()?;
  let state = if file.maybe_failure.is_some() || file.retry.is_stalled() {
    DraftFileState::Failed
  } else if !can_travel || !upload.is_traveling() || file.maybe_done.is_some() {
    // Up, but the drafts were full when it got there: it waits here like one that never travels.
    DraftFileState::OnThisDevice
  } else {
    DraftFileState::Uploading
  };
  Some(DraftFileItem {
    id: upload.client_message_id.to_hex(),
    file_name: file.file_name.clone(),
    media_type: file.media_type.clone(),
    size_bytes: file.size_bytes,
    maybe_thumbnail: file
      .maybe_thumbnail
      .as_ref()
      .map(|thumbnail| thumbnail.as_slice().to_vec()),
    state,
  })
}

fn uploaded_item(id: &str, attachment: &AttachmentRef) -> DraftFileItem {
  DraftFileItem {
    id: id.to_owned(),
    file_name: attachment.file_name.clone(),
    media_type: attachment.media_type.clone(),
    size_bytes: attachment.size_bytes,
    maybe_thumbnail: attachment
      .maybe_thumbnail
      .as_ref()
      .map(|thumbnail| thumbnail.as_slice().to_vec()),
    state: DraftFileState::Ready,
  }
}
