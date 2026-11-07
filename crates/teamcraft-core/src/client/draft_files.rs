//! Files of unsent drafts (#1220). A file attached to a draft goes up at once, announced as a
//! draft's file: the server holds it for the uploader and deletes it after
//! `draft_file_retention_days` unless it is sent. Once up, it moves into the encrypted account data
//! (e2ee.md §6.4) with its per-file key, so the user's other devices show it in the draft and send
//! it without uploading it again. Sending keeps it at the server; removing it deletes it there.
//!
//! A file waits on this device instead when the server holds no drafts' files (an older version)
//! or the drafts already carry [`MAX_DRAFT_FILES`]; it goes up with its message then.

use teamcraft_proto::{AttachmentId, ClientMessageId, ConversationId, ThreadId};

use super::Client;
use super::files::{
  PendingFile, PendingUpload, UploadPurpose, announced_attachment, pending_upload, queue_abandon,
  save_pending_upload,
};
use crate::account_data::{AccountData, DraftFile, MAX_DRAFT_FILES};
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::store::{AttachmentRef, LocalStore, Table};

/// Account record: how long the server holds drafts' files, in days, as it published it in
/// `GET /me`; `None` for servers that hold none.
pub const SERVER_DRAFT_RETENTION_KEY: &str = "server_draft_file_retention_days";

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// How long the server holds drafts' files, if it holds them at all.
  pub(crate) fn draft_file_retention_days(&self) -> Option<u32> {
    draft_file_retention_days(&self.store)
  }

  /// Keeps what the server published about drafts' files (`None`: it holds none).
  pub(super) fn store_draft_file_retention(&self, maybe_days: Option<u32>) -> Result<(), CoreError> {
    let recorded = records::get_json::<Option<u32>>(&self.store, Table::Account, SERVER_DRAFT_RETENTION_KEY)?;
    if recorded != Some(maybe_days) {
      records::put_json(
        &self.store,
        Table::Account,
        SERVER_DRAFT_RETENTION_KEY,
        &maybe_days,
      )?;
    }
    Ok(())
  }

  /// A draft's file finished: one that is up moves into the account data and leaves the pending
  /// uploads. One that failed, or that can't travel (the drafts are full), stays here for the
  /// composer; one removed on another device meanwhile is given up at the server.
  pub(super) fn complete_draft_file(
    &mut self,
    upload: &PendingUpload,
    maybe_thread: Option<ThreadId>,
    added_at_ms: i64,
  ) -> Result<(), CoreError> {
    let Some(attachment) = upload.files.iter().find_map(|file| file.maybe_done.clone()) else {
      return Ok(());
    };
    let id = upload.client_message_id.to_hex();
    let data = self.account_data();
    if data.draft_files.get(&id).is_some_and(|entry| entry.is_removed) {
      queue_abandon(&self.store, attachment.attachment_id)?;
      return self.forget_draft_upload(upload);
    }
    let file = DraftFile {
      conversation_id: upload.conversation_id,
      maybe_thread_id: maybe_thread,
      added_at_ms,
      attachment: attachment.clone(),
    };
    let mut is_added = false;
    self.update_account_data(|data, now| {
      is_added = data.add_draft_file(&id, file, now);
      is_added
    })?;
    if !is_added {
      return Ok(());
    }
    // The thumbnail may be too large to travel; this device keeps the whole reference.
    records::put_json(&self.store, Table::FileTransfers, &local_key(&id), &attachment)?;
    self.forget_draft_upload(upload)
  }

  /// Takes the files a message sends out of their drafts on every device (#1220).
  pub(crate) fn take_draft_files(&mut self, ids: &[String]) -> Result<(), CoreError> {
    self.update_account_data(|data, now| {
      let mut is_changed = false;
      for id in ids {
        is_changed |= data.take_draft_file(id, now).is_some();
      }
      is_changed
    })?;
    for id in ids {
      self.store.delete(Table::FileTransfers, &local_key(id))?;
    }
    Ok(())
  }

  /// Removes an uploaded file from its draft on every device and gives it up at the server.
  pub(crate) fn remove_draft_file(&mut self, id: &str) -> Result<(), CoreError> {
    let mut maybe_removed = None;
    self.update_account_data(|data, now| {
      maybe_removed = data.take_draft_file(id, now);
      maybe_removed.is_some()
    })?;
    self.store.delete(Table::FileTransfers, &local_key(id))?;
    if let Some(removed) = maybe_removed {
      queue_abandon(&self.store, removed.attachment.attachment_id)?;
    }
    Ok(())
  }

  fn forget_draft_upload(&mut self, upload: &PendingUpload) -> Result<(), CoreError> {
    self.store.delete(
      Table::PendingUploads,
      &records::outbox_key(upload.conversation_id, upload.client_message_id),
    )?;
    Ok(())
  }
}

/// How long the server holds drafts' files, as this device last read it.
pub(crate) fn draft_file_retention_days(store: &impl LocalStore) -> Option<u32> {
  records::get_json::<Option<u32>>(store, Table::Account, SERVER_DRAFT_RETENTION_KEY)
    .ok()
    .flatten()
    .flatten()
}

/// Attaches `files` to the draft, each as its own pending upload under a fresh ID. Files beyond what
/// the drafts carry ([`MAX_DRAFT_FILES`], counting those still going up) stay on this device.
pub fn attach_draft_files(
  store: &impl LocalStore,
  data: &AccountData,
  conversation: ConversationId,
  maybe_thread: Option<ThreadId>,
  files: Vec<PendingFile>,
  now_ms: i64,
) -> Result<Vec<ClientMessageId>, CoreError> {
  let mut traveling = data.live_draft_files().count()
    + draft_uploads(store)?
      .iter()
      .filter(|upload| upload.is_traveling())
      .count();
  let mut ids = Vec::with_capacity(files.len());
  for (index, file) in files.into_iter().enumerate() {
    let id = ClientMessageId::random();
    let is_local = traveling >= MAX_DRAFT_FILES;
    traveling += usize::from(!is_local);
    let upload = PendingUpload {
      conversation_id: conversation,
      client_message_id: id,
      files: vec![file],
      purpose: UploadPurpose::Draft {
        maybe_thread,
        // Several files attached at once keep their order.
        added_at_ms: now_ms.saturating_add(i64::try_from(index).unwrap_or(i64::MAX)),
        is_local,
      },
    };
    save_pending_upload(store, &upload)?;
    ids.push(id);
  }
  Ok(ids)
}

/// Where a draft's file is: still on this device (going up, failed or waiting for the send), or in
/// the account data.
pub enum DraftFileSource {
  Pending(PendingUpload),
  Uploaded(AttachmentRef),
}

/// Finds a draft's file by its ID. The uploading device's own copy of an uploaded one wins: its
/// thumbnail may be too large to travel.
pub fn find_draft_file(
  store: &impl LocalStore,
  data: &AccountData,
  conversation: ConversationId,
  id: &str,
) -> Result<Option<DraftFileSource>, CoreError> {
  if let Ok(client_message_id) = ClientMessageId::from_hex(id)
    && let Some(upload) = pending_upload(store, conversation, client_message_id)?
    && !upload.purpose.is_message()
  {
    return Ok(Some(DraftFileSource::Pending(upload)));
  }
  let Some(entry) = data.draft_files.get(id).filter(|entry| !entry.is_removed) else {
    return Ok(None);
  };
  if entry.value.conversation_id != conversation {
    return Ok(None);
  }
  let own = own_draft_file(store, id);
  Ok(Some(DraftFileSource::Uploaded(
    own.unwrap_or_else(|| entry.value.attachment.clone()),
  )))
}

/// Takes a draft's file that is still on this device out of the pending uploads and gives up what
/// the server has of it. Returns whether there was one.
pub fn remove_pending_draft_file(
  store: &impl LocalStore,
  conversation: ConversationId,
  id: &str,
) -> Result<bool, CoreError> {
  let Ok(client_message_id) = ClientMessageId::from_hex(id) else {
    return Ok(false);
  };
  let Some(upload) = pending_upload(store, conversation, client_message_id)? else {
    return Ok(false);
  };
  if upload.purpose.is_message() {
    return Ok(false);
  }
  for attachment in upload.files.iter().filter_map(file_attachment) {
    queue_abandon(store, attachment)?;
  }
  store.delete(
    Table::PendingUploads,
    &records::outbox_key(conversation, client_message_id),
  )?;
  Ok(true)
}

/// The pending uploads of drafts' files, of all conversations.
pub fn draft_uploads(store: &impl LocalStore) -> Result<Vec<PendingUpload>, CoreError> {
  Ok(
    records::list_json::<PendingUpload>(store, Table::PendingUploads)?
      .into_iter()
      .filter(|upload| !upload.purpose.is_message())
      .collect(),
  )
}

/// The attachment the server has for a pending file: announced, or up already.
pub(super) fn file_attachment(file: &PendingFile) -> Option<AttachmentId> {
  file
    .maybe_done
    .as_ref()
    .map(|done| done.attachment_id)
    .or_else(|| announced_attachment(file))
}

/// The uploading device's own copy of an uploaded draft's file, with its whole thumbnail.
pub fn own_draft_file(store: &impl LocalStore, id: &str) -> Option<AttachmentRef> {
  records::get_json(store, Table::FileTransfers, &local_key(id))
    .ok()
    .flatten()
}

fn local_key(id: &str) -> String {
  format!("draft_file/{id}")
}

impl PendingUpload {
  /// Whether this draft's file travels to the user's other devices once up.
  pub fn is_traveling(&self) -> bool {
    matches!(self.purpose, UploadPurpose::Draft { is_local: false, .. })
  }
}
