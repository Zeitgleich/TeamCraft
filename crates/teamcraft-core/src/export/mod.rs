//! "Export my data" (issue #211, docs/data-export.md): the user's own client writes a ZIP archive
//! of what it can read, since the server only holds ciphertext of messages and files (e2ee.md §4.4).
//! The archive holds what the server knows about the user that it can read
//! (`GET /me/personal-data`) and the account's security log, the account data of the key bag
//! without its keys, and per conversation the messages as NDJSON, read from the local store after
//! the history was fetched down to its beginning, plus the files the user sent.
//!
//! A host (the desktop shell, the core in the browser page) owns an [`ExportJob`] and calls
//! [`ExportJob::step`] between its other work, like the background history pages: each step does
//! one bounded thing (a request, a page of up to [`FEED_PAGE`] messages, one file), so sends and
//! gateway events go on and memory stays bounded by a page and a file. A network failure waits and
//! retries ([`ExportStep::RetryAt`]); a failure that retrying won't fix ends the export, and the
//! host deletes what it wrote.
//!
//! **Resuming.** After every whole entry of the archive the sink is flushed and an
//! [`ExportCheckpoint`] goes into the encrypted store: what was written, where the next step
//! starts, the counts. An app that stops in the middle finds it at the next start
//! ([`ExportJob::stored`]); the host cuts its file back to the checkpoint's length and goes on
//! ([`ExportJob::resume`]), or deletes the file ([`ExportJob::forget`]). An entry in the middle is
//! written again, so a conversation is cut into parts of [`MESSAGES_PER_PART`] messages.
//!
//! **Privacy.** The archive is plaintext: it goes where the user chose (the desktop's save dialog,
//! the browser's download) and nowhere else, and nothing of its content is logged. No key of the
//! user's devices, conversations or files is ever written.

mod format;
mod index;
mod zip;

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use teamcraft_client_api::data_export::{
  DataExportFailure, DataExportLabels, DataExportOptions, DataExportPhase, DataExportProgress,
  DataExportStatus,
};
use teamcraft_proto::{
  AuditEntryId, ConversationId, ConversationKind, ConversationResponse, Id, ListAuditQuery, ThreadId, UserId,
  UserSummary,
};

pub use format::{FORMAT, FORMAT_VERSION, ROOT};
pub use zip::{ExportSink, ZipEntry, ZipState, ZipWriter};

use crate::api::{ApiError, ServerApi};
use crate::client::Client;
use crate::client::files::attachments_of;
use crate::error::CoreError;
use crate::records;
use crate::store::{
  AttachmentRef, LocalStore, MessageContent, MessageFilter, MessageOrder, StoredMessage, Table,
};
use format::{
  AccountDataRecord, ConversationSummary, Counts, EarlierVersion, FileRecord, Manifest, ManifestFile,
  MessageRecord, MissingFile, MissingReason, ReactionRecord, ReplyRecord,
};

/// Rows of a conversation's feed read per step.
pub const FEED_PAGE: usize = 100;
/// Messages per part of a conversation: what a resume writes again at most.
pub const MESSAGES_PER_PART: u64 = 5_000;
/// The largest file that goes into the archive: one file is held in memory while it is written.
/// Bigger ones are listed as missing; the user saves them from the app.
pub const MAX_EXPORTED_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Entries of the security log read per request.
const SECURITY_LOG_PAGE: u32 = 100;
/// The record in [`Table::SyncState`] that holds the checkpoint.
const CHECKPOINT_KEY: &str = "data_export";
/// The first wait after a network failure, doubled up to [`RETRY_MAX_MS`].
const RETRY_FIRST_MS: i64 = 5_000;
const RETRY_MAX_MS: i64 = 5 * 60_000;

/// Why a step failed.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
  /// The sink refused the bytes (disk full, folder gone, no permission).
  #[error("the archive could not be written: {0}")]
  Sink(String),
  #[error(transparent)]
  Core(#[from] CoreError),
  /// A bug of the caller: entries out of order.
  #[error("export: {0}")]
  Invalid(&'static str),
  /// The place an earlier run wrote to doesn't match its checkpoint.
  #[error("the unfinished export can't continue: {0}")]
  NotResumable(String),
}

impl From<crate::store::StoreError> for ExportError {
  fn from(error: crate::store::StoreError) -> Self {
    Self::Core(error.into())
  }
}

impl From<ApiError> for ExportError {
  fn from(error: ApiError) -> Self {
    Self::Core(error.into())
  }
}

impl From<serde_json::Error> for ExportError {
  fn from(error: serde_json::Error) -> Self {
    Self::Core(CoreError::Corrupt(format!("export record: {error}")))
  }
}

impl ExportError {
  /// What the card tells the user.
  pub fn failure(&self) -> DataExportFailure {
    match self {
      Self::Sink(_) => DataExportFailure::Storage,
      Self::NotResumable(_) => DataExportFailure::NotResumable,
      Self::Core(CoreError::Api(_)) => DataExportFailure::Server,
      Self::Core(_) | Self::Invalid(_) => DataExportFailure::Other,
    }
  }

  /// Worth trying again later: the network, an overloaded server, a token about to be refreshed.
  fn is_retry_later(&self) -> bool {
    matches!(self, Self::Core(error) if error.is_retry_later())
  }
}

/// What one [`ExportJob::step`] left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportStep {
  /// More to do; call again when nothing else waits.
  Continue,
  /// The network or the server failed: call again at this time (client clock), or sooner when the
  /// connection is back.
  RetryAt(i64),
  /// The archive is complete and the checkpoint deleted; the host keeps the file.
  Done,
}

/// Everything a later run needs to go on: written with every whole entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportCheckpoint {
  /// Where the host writes the archive (a path on the desktop); the core doesn't read it.
  pub target: String,
  options: DataExportOptions,
  labels: DataExportLabels,
  user: UserId,
  created_at_ms: i64,
  zip: ZipState,
  stage: Stage,
  conversations: Vec<ConversationId>,
  summaries: Vec<ConversationSummary>,
  /// Files of the last closed part, written before anything else.
  pending_files: Vec<PendingFile>,
  missing_files: Vec<MissingFile>,
  counts: Counts,
}

/// Where the export goes on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
enum Stage {
  PersonalData,
  AccountData,
  SecurityLog,
  /// Fetching the conversation's older history into the store.
  History {
    conversation: usize,
  },
  /// Writing part `part` of the conversation, from after `after` on.
  Messages {
    conversation: usize,
    part: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    after: Option<OrderKey>,
  },
  Index,
  Manifest,
  Done,
}

/// A message's place in the feed ([`MessageOrder`]), as it is saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OrderKey {
  group: u8,
  value: i64,
  client_message_id: [u8; 16],
}

/// A file of the user's to download and write after the part that names it.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PendingFile {
  conversation: ConversationId,
  path: String,
  reference: AttachmentRef,
}

/// The entry being written; not in the checkpoint, which only knows whole entries.
enum OpenEntry {
  SecurityLog { maybe_before: Option<AuditEntryId> },
  Messages { after: Option<OrderKey>, written: u64 },
}

/// One export, from its start to the archive's last byte; see the module docs.
pub struct ExportJob {
  state: ExportCheckpoint,
  zip: ZipWriter,
  maybe_open: Option<OpenEntry>,
  retry_delay_ms: i64,
  maybe_retry_at_ms: Option<i64>,
}

impl ExportJob {
  /// Starts an export of `client`'s account: the conversations are the ones the user is a member
  /// of now. `target` is the host's, saved for a resume.
  pub fn start<A: ServerApi, S: LocalStore>(
    client: &Client<A, S>,
    options: DataExportOptions,
    labels: DataExportLabels,
    target: String,
  ) -> Result<Self, ExportError> {
    let mut conversations: Vec<ConversationResponse> =
      records::list_json::<ConversationResponse>(client.store(), Table::Conversations)?
        .into_iter()
        .filter(|conversation| conversation.maybe_my_membership.is_some())
        .collect();
    conversations.sort_by_key(|conversation| conversation.id);
    let created_at_ms = client.now_ms();
    tracing::info!(
      conversations = conversations.len(),
      scope = ?options.scope,
      includes_files = options.includes_files,
      "data export started"
    );
    Ok(Self::from_checkpoint(ExportCheckpoint {
      target,
      options,
      labels,
      user: client.user_id(),
      created_at_ms,
      zip: ZipState::default(),
      stage: Stage::PersonalData,
      conversations: conversations.iter().map(|conversation| conversation.id).collect(),
      summaries: Vec::new(),
      pending_files: Vec::new(),
      missing_files: Vec::new(),
      counts: Counts::default(),
    }))
  }

  /// The checkpoint an earlier run left, if one did.
  pub fn stored(store: &impl LocalStore) -> Result<Option<ExportCheckpoint>, ExportError> {
    Ok(records::get_json(store, Table::SyncState, CHECKPOINT_KEY)?)
  }

  /// Goes on from `checkpoint`. The host has cut its sink back to [`ExportCheckpoint::len`] bytes.
  pub fn resume(checkpoint: ExportCheckpoint) -> Self {
    tracing::info!(entries = checkpoint.zip.entries.len(), "data export resumed");
    Self::from_checkpoint(checkpoint)
  }

  /// Deletes the checkpoint: the export ended, was cancelled or discarded.
  pub fn forget(store: &impl LocalStore) -> Result<(), ExportError> {
    store.delete(Table::SyncState, CHECKPOINT_KEY)?;
    Ok(())
  }

  fn from_checkpoint(state: ExportCheckpoint) -> Self {
    let zip = ZipWriter::resume(state.zip.clone(), state.created_at_ms);
    Self {
      state,
      zip,
      maybe_open: None,
      retry_delay_ms: RETRY_FIRST_MS,
      maybe_retry_at_ms: None,
    }
  }

  /// Where the host writes the archive.
  pub fn target(&self) -> &str {
    &self.state.target
  }

  /// When the export was started (client clock).
  pub fn created_at_ms(&self) -> i64 {
    self.state.created_at_ms
  }

  /// When the next step is due after a failure; `None` while it can go on at once.
  pub fn retry_at_ms(&self) -> Option<i64> {
    self.maybe_retry_at_ms
  }

  /// The connection is back: a waiting export goes on now instead of at its retry time.
  pub fn network_back(&mut self) {
    self.maybe_retry_at_ms = None;
  }

  /// How far it got, for the card.
  pub fn status(&self) -> DataExportStatus {
    let progress = self.progress();
    match self.maybe_retry_at_ms {
      Some(retry_at_ms) => DataExportStatus::Waiting {
        progress,
        retry_at_ms,
      },
      None => DataExportStatus::Running { progress },
    }
  }

  pub fn progress(&self) -> DataExportProgress {
    self.state.progress(self.zip.len())
  }

  /// Does the next bounded piece of work. An `Err` ends the export: the host deletes what it
  /// wrote and calls [`Self::forget`].
  pub async fn step<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &mut Client<A, S>,
    sink: &mut dyn ExportSink,
  ) -> Result<ExportStep, ExportError> {
    if let Some(retry_at_ms) = self.maybe_retry_at_ms
      && client.now_ms() < retry_at_ms
    {
      return Ok(ExportStep::RetryAt(retry_at_ms));
    }
    match Box::pin(self.advance(client, sink)).await {
      Ok(step) => {
        self.maybe_retry_at_ms = None;
        self.retry_delay_ms = RETRY_FIRST_MS;
        Ok(step)
      },
      Err(error) if error.is_retry_later() => {
        let retry_at_ms = client.now_ms().saturating_add(self.retry_delay_ms);
        tracing::info!(%error, delay_ms = self.retry_delay_ms, "data export waits for the network");
        self.retry_delay_ms = self.retry_delay_ms.saturating_mul(2).min(RETRY_MAX_MS);
        self.maybe_retry_at_ms = Some(retry_at_ms);
        Ok(ExportStep::RetryAt(retry_at_ms))
      },
      Err(error) => {
        tracing::warn!(%error, "data export failed");
        Err(error)
      },
    }
  }

  async fn advance<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &mut Client<A, S>,
    sink: &mut dyn ExportSink,
  ) -> Result<ExportStep, ExportError> {
    if self.maybe_open.is_none() && !self.state.pending_files.is_empty() {
      Box::pin(self.write_pending_file(client, sink)).await?;
      return Ok(ExportStep::Continue);
    }
    match self.state.stage.clone() {
      Stage::PersonalData => {
        let data = client.api().personal_data().await?;
        let bytes = serde_json::to_vec_pretty(&data)?;
        self.zip.add(sink, &format::personal_data_path(), &bytes)?;
        self.close(client, sink, Stage::AccountData)?;
      },
      Stage::AccountData => {
        let data = client.account_data();
        let bytes = serde_json::to_vec_pretty(&AccountDataRecord::of(&data))?;
        self.zip.add(sink, &format::account_data_path(), &bytes)?;
        self.close(client, sink, Stage::SecurityLog)?;
      },
      Stage::SecurityLog => Box::pin(self.write_security_log_page(client, sink)).await?,
      Stage::History { conversation } => Box::pin(self.read_history(client, sink, conversation)).await?,
      Stage::Messages {
        conversation,
        part,
        after,
      } => self.write_messages_page(client, sink, conversation, part, after)?,
      Stage::Index => {
        let summaries = serde_json::to_vec_pretty(&self.state.summaries)?;
        self.zip.add(sink, &format::conversations_path(), &summaries)?;
        let page = index::page(&self.state.labels, &self.state, client.now_ms());
        self.zip.add(sink, &format::index_path(), page.as_bytes())?;
        self.close(client, sink, Stage::Manifest)?;
      },
      Stage::Manifest => {
        self.write_manifest(sink)?;
        let zip = std::mem::take(&mut self.zip);
        let length = zip.finish(sink)?;
        self.state.stage = Stage::Done;
        Self::forget(client.store())?;
        tracing::info!(
          bytes = length,
          messages = self.state.counts.messages,
          files = self.state.counts.files,
          "data export complete"
        );
        return Ok(ExportStep::Done);
      },
      Stage::Done => return Ok(ExportStep::Done),
    }
    Ok(ExportStep::Continue)
  }

  /// Ends the step after a whole entry: the sink is made durable, then the checkpoint names it,
  /// in that order, so the checkpoint never claims more than the sink holds.
  fn close<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &Client<A, S>,
    sink: &mut dyn ExportSink,
    next: Stage,
  ) -> Result<(), ExportError> {
    self.state.stage = next;
    self.state.zip = self.zip.state().clone();
    sink.flush()?;
    records::put_json(client.store(), Table::SyncState, CHECKPOINT_KEY, &self.state)?;
    Ok(())
  }

  async fn write_security_log_page<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &mut Client<A, S>,
    sink: &mut dyn ExportSink,
  ) -> Result<(), ExportError> {
    let maybe_before = match &self.maybe_open {
      Some(OpenEntry::SecurityLog { maybe_before }) => *maybe_before,
      _ => None,
    };
    let query = ListAuditQuery {
      maybe_before,
      maybe_limit: Some(SECURITY_LOG_PAGE),
      ..ListAuditQuery::default()
    };
    // Asked before anything is written, so a failed request leaves the entry as it was.
    let page = client.api().security_log(&query).await?;
    if self.maybe_open.is_none() {
      self.zip.begin(sink, &format::security_log_path())?;
    }
    let mut lines = Vec::new();
    for entry in &page.entries {
      serde_json::to_writer(&mut lines, entry)?;
      lines.push(b'\n');
    }
    self.zip.write(sink, &lines)?;
    match page.entries.last() {
      Some(last) if page.has_more => {
        self.maybe_open = Some(OpenEntry::SecurityLog {
          maybe_before: Some(last.id),
        });
      },
      _ => {
        self.zip.end(sink)?;
        self.maybe_open = None;
        let next = self.after_conversation(0);
        self.close(client, sink, next)?;
      },
    }
    Ok(())
  }

  /// The first stage of conversation `index`, or the index once all are written.
  fn after_conversation(&self, index: usize) -> Stage {
    if index < self.state.conversations.len() {
      Stage::History { conversation: index }
    } else {
      Stage::Index
    }
  }

  async fn read_history<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &mut Client<A, S>,
    sink: &mut dyn ExportSink,
    index: usize,
  ) -> Result<(), ExportError> {
    let Some(&conversation) = self.state.conversations.get(index) else {
      return self.close(client, sink, Stage::Index);
    };
    if self.summary_mut(conversation).is_none() {
      let summary = summary_of(client.store(), conversation, client.user_id())?;
      self.state.summaries.push(summary);
    }
    let is_at_beginning = match client.read_older_page_for_export(conversation).await {
      Ok(is_at_beginning) => is_at_beginning,
      // Not allowed to read further back (history from joining on), or gone meanwhile: what the
      // store holds is what there is.
      Err(CoreError::Api(ApiError::Status {
        status: 403 | 404, ..
      })) => {
        if let Some(summary) = self.summary_mut(conversation) {
          summary.is_history_limited = true;
        }
        true
      },
      Err(error) => return Err(error.into()),
    };
    if is_at_beginning {
      self.state.stage = Stage::Messages {
        conversation: index,
        part: 1,
        after: None,
      };
    }
    Ok(())
  }

  /// One page of the conversation's feed, with the thread replies and reactions of its messages.
  fn write_messages_page<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &Client<A, S>,
    sink: &mut dyn ExportSink,
    index: usize,
    part: u32,
    part_start: Option<OrderKey>,
  ) -> Result<(), ExportError> {
    let Some(&conversation) = self.state.conversations.get(index) else {
      return self.close(client, sink, Stage::Index);
    };
    let (after, written) = match &self.maybe_open {
      Some(OpenEntry::Messages { after, written }) => (*after, *written),
      _ => (part_start, 0),
    };
    let store = client.store();
    let me = client.user_id();
    let start = match after {
      Some(key) => store
        .feed_ranks(conversation, &[key.order()])?
        .first()
        .copied()
        .unwrap_or(0),
      None => 0,
    };
    let rows: Vec<StoredMessage> = store
      .feed_rows(conversation, start, FEED_PAGE + 1)?
      .into_iter()
      .filter(|row| after.is_none_or(|key| row.order() > key.order()))
      .take(FEED_PAGE)
      .collect();
    let Some(last) = rows.last() else {
      return self.end_conversation(client, sink, index);
    };
    let next_after = Some(OrderKey::of(last.order()));
    let threads = replies_of(store, conversation, &rows)?;
    let mut targets: Vec<_> = rows.iter().map(|row| row.client_message_id).collect();
    targets.extend(threads.values().flatten().map(|reply| reply.client_message_id));
    let reactions = reactions_of(store, conversation, &targets)?;
    let mut names = Names::default();
    let mut lines = Vec::new();
    let mut count = 0u64;
    for row in &rows {
      let replies = row
        .maybe_server_id
        .and_then(|id| threads.get(&ThreadId::from_snowflake(id.snowflake())));
      for message in std::iter::once(row).chain(replies.into_iter().flatten()) {
        if !format::is_in_scope(self.state.options.scope, message, me) {
          continue;
        }
        let files = self.files_of(conversation, message, me);
        let record = message_record(
          message,
          me,
          names.of(store, message.sender_user_id),
          files,
          &reactions,
        );
        serde_json::to_writer(&mut lines, &record)?;
        lines.push(b'\n');
        count += 1;
        if message.maybe_unreadable.is_some() {
          self.state.counts.unreadable_messages += 1;
        }
      }
    }
    if !lines.is_empty() {
      if self.maybe_open.is_none() {
        let path = format::messages_path(conversation, part);
        self.zip.begin(sink, &path)?;
        if let Some(summary) = self.summary_mut(conversation) {
          summary.parts.push(path);
        }
      }
      self.zip.write(sink, &lines)?;
      self.state.counts.messages += count;
      if let Some(summary) = self.summary_mut(conversation) {
        summary.messages += count;
      }
    }
    let written = written + count;
    if self.zip.is_open() && written >= MESSAGES_PER_PART {
      self.zip.end(sink)?;
      self.maybe_open = None;
      let next = Stage::Messages {
        conversation: index,
        part: part + 1,
        after: next_after,
      };
      return self.close(client, sink, next);
    }
    if self.zip.is_open() {
      self.maybe_open = Some(OpenEntry::Messages {
        after: next_after,
        written,
      });
    } else {
      // Nothing of this page was in scope: the part hasn't begun, and goes on from here.
      self.maybe_open = None;
      self.state.stage = Stage::Messages {
        conversation: index,
        part,
        after: next_after,
      };
      // There is no entry to close: the checkpoint moves with the next one, and a restart looks
      // at these rows again.
    }
    Ok(())
  }

  /// The conversation's last part ends; its files follow, then the next conversation.
  fn end_conversation<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &Client<A, S>,
    sink: &mut dyn ExportSink,
    index: usize,
  ) -> Result<(), ExportError> {
    if self.zip.is_open() {
      self.zip.end(sink)?;
    }
    self.maybe_open = None;
    self.state.counts.conversations += 1;
    let next = self.after_conversation(index + 1);
    self.close(client, sink, next)
  }

  /// The files of `message` as its record lists them; the user's own go into the archive (after
  /// the part ends) when the user chose files and they aren't too large.
  fn files_of<'a>(
    &mut self,
    conversation: ConversationId,
    message: &'a StoredMessage,
    me: UserId,
  ) -> Vec<FileRecord<'a>> {
    if message.maybe_unreadable.is_some() {
      return Vec::new();
    }
    attachments_of(&message.content)
      .into_iter()
      .map(|reference| {
        let is_taken = self.state.options.includes_files && message.sender_user_id == me;
        let maybe_path = if !is_taken {
          None
        } else if reference.size_bytes > MAX_EXPORTED_FILE_BYTES {
          self.note_missing(conversation, reference, MissingReason::TooLarge);
          None
        } else {
          let path = format::file_path(conversation, reference.attachment_id, &reference.file_name);
          self.state.pending_files.push(PendingFile {
            conversation,
            path: path.clone(),
            reference: reference.clone(),
          });
          Some(path)
        };
        FileRecord {
          attachment_id: reference.attachment_id,
          name: &reference.file_name,
          media_type: &reference.media_type,
          size_bytes: reference.size_bytes,
          path: maybe_path,
        }
      })
      .collect()
  }

  fn note_missing(&mut self, conversation: ConversationId, reference: &AttachmentRef, reason: MissingReason) {
    self.state.counts.files_missing += 1;
    self.state.missing_files.push(MissingFile {
      conversation_id: conversation,
      attachment_id: reference.attachment_id,
      file_name: reference.file_name.clone(),
      reason,
    });
  }

  /// Downloads, checks and decrypts the next waiting file and writes it as one entry.
  async fn write_pending_file<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &mut Client<A, S>,
    sink: &mut dyn ExportSink,
  ) -> Result<(), ExportError> {
    let Some(file) = self.state.pending_files.first().cloned() else {
      return Ok(());
    };
    let downloaded = client
      .download_decrypted_capped(&file.reference, Some(MAX_EXPORTED_FILE_BYTES))
      .await;
    self.state.pending_files.remove(0);
    match downloaded {
      Ok(plaintext) => {
        self.zip.add(sink, &file.path, plaintext.expose_secret())?;
        self.state.counts.files += 1;
        if let Some(summary) = self.summary_mut(file.conversation) {
          summary.files += 1;
        }
      },
      Err(error) if error.is_retry_later() => {
        // Tried again first thing after the wait.
        self.state.pending_files.insert(0, file);
        return Err(error.into());
      },
      Err(error) => {
        let reason = match &error {
          CoreError::Api(ApiError::Status {
            status: 403 | 404 | 410,
            ..
          }) => MissingReason::Gone,
          CoreError::Api(ApiError::BlobTooLarge { .. }) => MissingReason::TooLarge,
          _ => MissingReason::Damaged,
        };
        tracing::info!(%error, ?reason, "a file of the data export left out");
        self.note_missing(file.conversation, &file.reference, reason);
      },
    }
    let stage = self.state.stage.clone();
    self.close(client, sink, stage)
  }

  fn write_manifest(&mut self, sink: &mut dyn ExportSink) -> Result<(), ExportError> {
    let entries = self.zip.state().entries.clone();
    let manifest = Manifest {
      format: FORMAT,
      version: FORMAT_VERSION,
      created_at_ms: self.state.created_at_ms,
      user_id: self.state.user,
      scope: self.state.options.scope,
      includes_files: self.state.options.includes_files,
      max_file_bytes: MAX_EXPORTED_FILE_BYTES,
      counts: self.state.counts,
      files: entries
        .iter()
        .map(|entry| ManifestFile {
          path: &entry.name,
          size: entry.size,
          sha256: &entry.sha256,
        })
        .collect(),
      missing_files: &self.state.missing_files,
      not_included: format::NOT_INCLUDED,
    };
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    self.zip.add(sink, &format::manifest_path(), &bytes)
  }

  fn summary_mut(&mut self, conversation: ConversationId) -> Option<&mut ConversationSummary> {
    self
      .state
      .summaries
      .iter_mut()
      .find(|summary| summary.id == conversation)
  }
}

impl ExportCheckpoint {
  /// The archive's length at the checkpoint: what a host cuts its file back to before resuming.
  pub fn len(&self) -> u64 {
    self.zip.offset
  }

  pub fn is_empty(&self) -> bool {
    self.zip.offset == 0
  }

  /// How far the export had got, for the card of an unfinished one.
  pub fn progress(&self, bytes: u64) -> DataExportProgress {
    let maybe_phase = match self.stage {
      _ if !self.pending_files.is_empty() => DataExportPhase::Files,
      Stage::PersonalData | Stage::AccountData | Stage::SecurityLog => DataExportPhase::Account,
      Stage::History { .. } => DataExportPhase::History,
      Stage::Messages { .. } => DataExportPhase::Messages,
      Stage::Index | Stage::Manifest | Stage::Done => DataExportPhase::Finishing,
    };
    DataExportProgress {
      maybe_phase: Some(maybe_phase),
      conversations_done: self.counts.conversations,
      conversations_total: u32::try_from(self.conversations.len()).unwrap_or(u32::MAX),
      messages: self.counts.messages,
      files: self.counts.files,
      files_skipped: self.counts.files_missing,
      bytes,
    }
  }
}

impl OrderKey {
  fn of(order: MessageOrder) -> Self {
    Self {
      group: order.group,
      value: order.value,
      client_message_id: order.client_message_id,
    }
  }

  fn order(self) -> MessageOrder {
    MessageOrder {
      group: self.group,
      value: self.value,
      client_message_id: self.client_message_id,
    }
  }
}

/// The thread replies of `rows`, by thread, in order. Replies that also show in the conversation
/// are rows themselves and not repeated.
fn replies_of(
  store: &impl LocalStore,
  conversation: ConversationId,
  rows: &[StoredMessage],
) -> Result<HashMap<ThreadId, Vec<StoredMessage>>, ExportError> {
  let threads: Vec<ThreadId> = rows
    .iter()
    .filter_map(|row| row.maybe_server_id)
    .map(|id| ThreadId::from_snowflake(id.snowflake()))
    .collect();
  let counts = store.reply_counts(conversation, &threads)?;
  let mut replies = HashMap::new();
  for (thread, count) in threads.into_iter().zip(counts) {
    if count == 0 {
      continue;
    }
    let mut messages: Vec<StoredMessage> = store
      .messages(conversation, MessageFilter::Thread(thread))?
      .into_iter()
      .filter(|reply| {
        !reply.is_hidden() && !reply.is_also_in_channel() && !matches!(reply.content, MessageContent::Deleted)
      })
      .collect();
    messages.sort_by_key(StoredMessage::order);
    replies.insert(thread, messages);
  }
  Ok(replies)
}

/// Who reacted with what to each of `targets`: the latest reaction message of a person and emoji
/// decides whether it stands.
fn reactions_of(
  store: &impl LocalStore,
  conversation: ConversationId,
  targets: &[teamcraft_proto::ClientMessageId],
) -> Result<HashMap<teamcraft_proto::ClientMessageId, Vec<ReactionRecord>>, ExportError> {
  let mut standing: HashMap<teamcraft_proto::ClientMessageId, BTreeMap<String, BTreeMap<UserId, bool>>> =
    HashMap::new();
  // In display order, so a later message overrides an earlier one.
  for reaction in store.reactions_to(conversation, targets)? {
    if reaction.maybe_unreadable.is_some() {
      continue;
    }
    if let MessageContent::Reaction {
      target,
      emoji,
      is_added,
    } = &reaction.content
    {
      standing
        .entry(*target)
        .or_default()
        .entry(emoji.clone())
        .or_default()
        .insert(reaction.sender_user_id, *is_added);
    }
  }
  Ok(
    standing
      .into_iter()
      .map(|(target, by_emoji)| {
        let records = by_emoji
          .into_iter()
          .filter_map(|(emoji, users)| {
            let user_ids: Vec<UserId> = users
              .into_iter()
              .filter_map(|(user, is_added)| is_added.then_some(user))
              .collect();
            (!user_ids.is_empty()).then_some(ReactionRecord { emoji, user_ids })
          })
          .collect();
        (target, records)
      })
      .collect(),
  )
}

/// The record of one message.
fn message_record<'a>(
  message: &'a StoredMessage,
  me: UserId,
  maybe_sender_name: Option<&'a str>,
  files: Vec<FileRecord<'a>>,
  reactions: &HashMap<teamcraft_proto::ClientMessageId, Vec<ReactionRecord>>,
) -> MessageRecord<'a> {
  let (kind, maybe_text) = format::kind_and_text(message);
  let mut record = MessageRecord {
    id: message.maybe_server_id,
    client_message_id: message.client_message_id,
    thread_id: message.maybe_thread_id,
    sender_id: message.sender_user_id,
    sender_name: maybe_sender_name,
    is_own: message.sender_user_id == me,
    sent_at_ms: message.sent_at_ms,
    kind,
    text: maybe_text,
    is_edited: message.is_edited,
    earlier_versions: message
      .edits
      .iter()
      .map(|edit| EarlierVersion {
        text: &edit.body,
        replaced_at_ms: edit.replaced_at_ms,
      })
      .collect(),
    mentions: &[],
    priority: None,
    reply_to: None,
    forwarded_from: None,
    link: None,
    files,
    reactions: reactions
      .get(&message.client_message_id)
      .map(|records| {
        records
          .iter()
          .map(|reaction| ReactionRecord {
            emoji: reaction.emoji.clone(),
            user_ids: reaction.user_ids.clone(),
          })
          .collect()
      })
      .unwrap_or_default(),
    is_pinned: message.is_pinned,
    event: None,
    unreadable: message.maybe_unreadable,
  };
  if message.maybe_unreadable.is_some() {
    return record;
  }
  match &message.content {
    MessageContent::Text { mentions, meta, .. } => {
      record.mentions = mentions;
      record.priority = (meta.priority != crate::store::Priority::Normal).then_some(meta.priority);
      record.reply_to = meta.maybe_reply_to.as_ref().map(|reply| ReplyRecord {
        client_message_id: reply.target,
        author_id: reply.author,
        excerpt: &reply.excerpt,
      });
      record.forwarded_from = meta
        .maybe_forwarded
        .as_ref()
        .map(|forwarded| forwarded.author_name.as_str());
      record.link = meta
        .maybe_link_preview
        .as_ref()
        .map(|preview| preview.url.as_str());
    },
    MessageContent::Event { event } => record.event = serde_json::to_value(event).ok(),
    _ => {},
  }
  record
}

/// The summary of a conversation before its messages are written.
fn summary_of(
  store: &impl LocalStore,
  conversation: ConversationId,
  me: UserId,
) -> Result<ConversationSummary, ExportError> {
  let maybe_record: Option<ConversationResponse> =
    records::get_json(store, Table::Conversations, &conversation.to_string())?;
  let (kind, name) = match &maybe_record {
    Some(record) => (kind_name(&record.kind), conversation_name(record, me)),
    None => ("unknown".to_owned(), String::new()),
  };
  Ok(ConversationSummary {
    id: conversation,
    kind,
    name,
    parts: Vec::new(),
    messages: 0,
    files: 0,
    is_history_limited: false,
  })
}

fn kind_name(kind: &ConversationKind) -> String {
  serde_json::to_value(kind)
    .ok()
    .and_then(|value| value.as_str().map(str::to_owned))
    .unwrap_or_else(|| "unknown".to_owned())
}

/// A channel's name, or the other participants of a direct conversation.
fn conversation_name(record: &ConversationResponse, me: UserId) -> String {
  if let Some(name) = record.maybe_name.as_ref().filter(|name| !name.is_empty()) {
    return name.clone();
  }
  let others: Vec<&str> = record
    .participants
    .iter()
    .filter(|participant| participant.id != me)
    .map(|participant| participant.display_name.as_str())
    .collect();
  if others.is_empty() {
    record
      .participants
      .first()
      .map(|participant| participant.display_name.clone())
      .unwrap_or_default()
  } else {
    others.join(", ")
  }
}

/// Senders' names, read once per page from the store's people.
#[derive(Default)]
struct Names(HashMap<UserId, Option<String>>);

impl Names {
  fn of(&mut self, store: &impl LocalStore, user: UserId) -> Option<&str> {
    self
      .0
      .entry(user)
      .or_insert_with(|| {
        records::get_json::<UserSummary>(store, Table::Users, &user.to_string())
          .ok()
          .flatten()
          .map(|summary| summary.display_name)
      })
      .as_deref()
  }
}

/// A sink that keeps the archive in memory: for tests, and small archives.
#[derive(Default)]
pub struct MemorySink {
  pub bytes: Vec<u8>,
  pub flushes: usize,
}

impl ExportSink for MemorySink {
  fn write_all(&mut self, bytes: &[u8]) -> Result<(), ExportError> {
    self.bytes.extend_from_slice(bytes);
    Ok(())
  }

  fn flush(&mut self) -> Result<(), ExportError> {
    self.flushes += 1;
    Ok(())
  }
}

/// The archive's name the save dialog suggests: `teamcraft-export-<date>.zip`, the date of
/// `created_at_ms` (UTC).
pub fn suggested_file_name(created_at_ms: i64) -> String {
  let (year, month, day) = zip::civil_from_days(created_at_ms.div_euclid(86_400_000));
  format!("teamcraft-export-{year:04}-{month:02}-{day:02}.zip")
}
