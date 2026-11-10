//! A store that notes which parts of the UI's snapshot its writes may have changed, so the snapshot
//! is rebuilt per conversation instead of reading the whole workspace on every change (issue #246).
//!
//! The engine and the UI adapter share one [`TrackedStore`]; the host takes the [`StoreChanges`]
//! when the UI asks what changed ([`crate::ui_snapshot::SnapshotCache`]). Writes are classified by
//! table and key: what one conversation's sidebar entry or feed reads, what one person's entry
//! reads, the live state (counts, threads, presence log, receipts), the security part, or what
//! every conversation may depend on (names, spaces, preferences). Tables the snapshot doesn't read
//! (MLS state, sync cursors, search segments, …) are not noted at all.
//!
//! With [`TrackedStore::with_message_cache`] it keeps the decoded messages of the conversations
//! read last and a revision per conversation ([`MessageCache`], issue #1000).
//!
//! For a host with a persistent media cache ([`TrackedStore::with_media_cache`]) it also notes what
//! goes from that cache (#749): the image attachments of a message deleted or withdrawn by its
//! sender ([`TrackedStore::take_removed_renditions`]), and every picture of a conversation that is
//! no longer listed for this user or whose history goes (purged, e2ee.md §4.6;
//! [`TrackedStore::take_media_purges`]). The latter is a record in the same group of writes, so a
//! purge interrupted by a crash is finished by the next start (#1161).
//!
//! Over a store whose commits are numbered ([`LocalStore::last_commit`]: the app's writing
//! connection beside reading ones, issue #1476), the notes and the journal of a group of writes
//! are published only once it committed, and a reader whose snapshot is older than what it takes
//! takes a new one first ([`LocalStore::renew_snapshot`]): it never takes a note of a write its
//! snapshot doesn't hold, and keeps nothing derived from a conversation the open group changes.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, ThreadId as ThreadIdStd};

use teamcraft_proto::{AttachmentId, ClientMessageId, ConversationId, MessageId, ThreadId, UserId};

use super::message_cache::MessageCache;
use super::{
  Attached, AttachedRange, AttachmentRef, FeedVersion, LocalStore, MessageContent, MessageEdge,
  MessageFilter, MessageOrder, MessageSlot, StoreError, StoredMessage, Table, WriteTally, Written,
};
use crate::records::draft_reply_key;

/// What the UI's message cache may hold by default (issue #1125): about 60,000 messages at the
/// 1 KB a message of ordinary length takes decoded (docs/performance.md, "Local store"), but
/// counted in bytes, so a channel of long messages doesn't hold more. Within the mobile budget of
/// 250 MiB together with the store's own caches ([`super::SealedStoreLimits`]).
pub const DEFAULT_MESSAGE_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// Records of [`Table::Account`] every conversation entry reads (unread marks, mention rules).
const SHARED_ACCOUNT_RECORDS: [&str; 2] = [
  crate::client::ACCOUNT_DATA_KEY,
  crate::notify_rules::MENTION_PREFERENCES_RECORD,
];
/// The key prefix of a conversation's bookmarks in [`Table::Account`].
const BOOKMARKS_PREFIX: &str = "bookmarks/";
/// Removed renditions kept until the host takes them; a host that doesn't take them for long
/// doesn't make them grow, the oldest go (the cache's age limit covers what is lost). A whole
/// conversation never goes through them ([`MEDIA_PURGE_PREFIX`]).
const MAX_REMOVED_RENDITIONS: usize = 4096;
/// Records in [`Table::SyncState`], `media_purge/{conversation}`: the conversation's pictures are
/// still to be dropped from the media cache. One per conversation, deleted once the host did.
const MEDIA_PURGE_PREFIX: &str = "media_purge/";
/// Most looks a purge the host failed is skipped before it is handed out again; the skips double
/// with each failure up to this.
const MAX_MEDIA_PURGE_SKIPS: u32 = 64;

/// A [`LocalStore`] that notes what its writes changed. Clones share the notes.
#[derive(Clone)]
pub struct TrackedStore<S> {
  inner: S,
  changes: Arc<Mutex<StoreChanges>>,
  removed_renditions: Arc<Mutex<VecDeque<RemovedRendition>>>,
  messages: Arc<Mutex<MessageCache>>,
  /// The host keeps a media cache: renditions of deleted messages are noted.
  has_media_cache: bool,
  /// The host keeps files of conversations (a media cache, drafts): conversations that went are
  /// noted.
  notes_conversation_purges: bool,
  media_purges: Arc<Mutex<MediaPurges>>,
  /// The messages written through this store, newest last ([`LocalStore::written_since`]).
  journal: Arc<Mutex<Journal>>,
  /// [`WriteTally::foreign`] when the changes were last taken.
  seen_foreign: Arc<AtomicU64>,
  /// Commits are numbered: notes of a group wait for its commit ([`GroupNotes`]).
  is_numbered: bool,
  /// The notes of the open group of writes, until it ends.
  group: Arc<Mutex<GroupNotes>>,
  /// The number of the newest commit whose notes were published: a reader's snapshot must hold it
  /// before the reader takes them.
  published: Arc<AtomicU64>,
}

/// The notes of the open group of writes over a store with numbered commits, held back until the
/// group ends: a reader on another connection would take them before its snapshot can hold the
/// writes.
#[derive(Debug, Default)]
struct GroupNotes {
  /// The thread whose group is open; the store lets no other thread write meanwhile.
  maybe_owner: Option<ThreadIdStd>,
  depth: u32,
  changes: Vec<Change>,
  /// Messages written, and whether the write may have added or taken away a file.
  written: Vec<(ConversationId, Written, bool)>,
  /// Conversations the group wrote to: a reader keeps nothing derived from them meanwhile.
  conversations: BTreeSet<ConversationId>,
}

/// The newest writes of messages, by number: a reader that keeps its own number since asks what
/// came after it. Bounded; what fell out, and whatever another connection wrote, it can't tell.
#[derive(Debug, Default)]
struct Journal {
  entries: VecDeque<(u64, ConversationId, Written)>,
  /// The number the next write gets.
  next: u64,
  /// It can tell of the writes from this number on.
  complete_from: u64,
  /// [`WriteTally::foreign`] when last looked at.
  seen_foreign: u64,
  /// Counts writes that may have added or taken away a file ([`LocalStore::attachments_cursor`]).
  attachments: u64,
}

/// The host's work on [`MEDIA_PURGE_PREFIX`] records.
#[derive(Debug)]
struct MediaPurges {
  /// Records may wait that aren't handed out: at start (left by an earlier run), after one was
  /// written or a purge failed. Otherwise the host's look costs nothing.
  is_due: bool,
  /// Handed out, not yet confirmed or failed: not handed out again meanwhile.
  in_flight: BTreeSet<ConversationId>,
  /// Purges that failed, by how many looks they still sit out and how often they failed.
  backoff: HashMap<ConversationId, Backoff>,
}

#[derive(Clone, Copy, Debug)]
struct Backoff {
  failures: u32,
  skips_left: u32,
}

/// An image attachment whose message went away: its cached rendition goes too. Identifies the entry
/// like `teamcraft_client_api::media::MediaKey::Rendition`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemovedRendition {
  pub conversation: ConversationId,
  pub message: MessageId,
  pub attachment: AttachmentId,
  pub content_sha256: [u8; 32],
}

/// What changed since the changes were last taken.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreChanges {
  /// Conversations whose entry or feed may differ: messages, members, bookmarks, acknowledgements.
  pub conversations: BTreeSet<ConversationId>,
  /// People whose entry may differ (their presence state or custom status).
  pub users: BTreeSet<UserId>,
  /// People whose record in the presence log was written or dropped (also counts as live), so a
  /// reader's index of the log follows without reading all of it (issue #259).
  pub announced: BTreeSet<UserId>,
  /// Unread counts, threads, the presence log, receipts or the connection marks.
  pub is_live: bool,
  /// Pinned identity keys or the verified workspace.
  pub is_security: bool,
  /// Something every entry may depend on: people's names, spaces, the conversation list,
  /// categories, the workspace, account data, mention rules.
  pub is_everything: bool,
  /// Another process committed to the store (the iOS notification extension): it says nothing of
  /// what it wrote, so every part of the snapshot and every feed is read again. Comes with the
  /// three marks above.
  pub is_foreign: bool,
}

/// What one write changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
  Conversation(ConversationId),
  User(UserId),
  /// A person's record in the presence log.
  Announced(UserId),
  Live,
  Security,
  Everything,
  /// Workspace: both the security part and everything else.
  Workspace,
  Nothing,
}

impl GroupNotes {
  fn is_mine(&self) -> bool {
    self.depth > 0 && self.maybe_owner == Some(thread::current().id())
  }

  /// The owner's group ends one level; the notes once it ended.
  fn end(&mut self) -> Option<Self> {
    if !self.is_mine() {
      return None;
    }
    self.depth -= 1;
    (self.depth == 0).then(|| std::mem::take(self))
  }
}

impl Journal {
  /// Writes kept at most; older ones are no longer told apart.
  const MAX_ENTRIES: usize = 8_192;

  /// Noted before the write: a reader that asks meanwhile reads the message again, which costs a
  /// read and is right either way.
  fn note(&mut self, conversation: ConversationId, written: Written) {
    self.entries.push_back((self.next, conversation, written));
    self.next += 1;
    if self.entries.len() > Self::MAX_ENTRIES
      && let Some((dropped, _, _)) = self.entries.pop_front()
    {
      self.complete_from = dropped + 1;
    }
  }

  fn follow_disk(&mut self, maybe_tally: Option<WriteTally>) {
    if let Some(tally) = maybe_tally
      && tally.foreign != self.seen_foreign
    {
      self.seen_foreign = tally.foreign;
      // What the other connection wrote isn't here: nobody's number before now holds.
      self.complete_from = self.next;
      self.attachments += 1;
    }
  }

  fn since(&self, conversation: ConversationId, cursor: u64) -> Option<Vec<Written>> {
    if cursor < self.complete_from || cursor > self.next {
      return None;
    }
    Some(
      self
        .entries
        .iter()
        .filter(|(number, held, _)| *number >= cursor && *held == conversation)
        .map(|(_, _, written)| *written)
        .collect(),
    )
  }
}

impl<S: LocalStore> TrackedStore<S> {
  pub fn new(inner: S) -> Self {
    Self::with_message_cache(inner, 0)
  }

  /// Also keeps the decoded messages of the conversations read last, up to `max_size_bytes` of
  /// memory in all ([`DEFAULT_MESSAGE_CACHE_BYTES`] is the usual figure), for a store whose reads
  /// are expensive (SQLCipher decrypts and checks every page it reads). Stores that already hold
  /// everything in memory gain nothing from it. A conversation larger than the whole budget is
  /// never kept; `0` keeps none.
  pub fn with_message_cache(inner: S, max_size_bytes: usize) -> Self {
    Self {
      is_numbered: inner.last_commit().is_some(),
      inner,
      changes: Arc::default(),
      removed_renditions: Arc::default(),
      messages: Arc::new(Mutex::new(MessageCache::new(max_size_bytes))),
      has_media_cache: false,
      notes_conversation_purges: false,
      media_purges: Arc::new(Mutex::new(MediaPurges {
        is_due: true,
        in_flight: BTreeSet::new(),
        backoff: HashMap::new(),
      })),
      journal: Arc::default(),
      seen_foreign: Arc::default(),
      group: Arc::default(),
      published: Arc::default(),
    }
  }

  /// The same notes, journal and caches over another view of the same store: a read thread's
  /// [`super::LaneStore`] beside the engine's.
  pub fn sharing(&self, inner: S) -> Self
  where
    S: Clone,
  {
    Self {
      is_numbered: inner.last_commit().is_some(),
      inner,
      ..self.clone()
    }
  }

  /// The host keeps a persistent media cache: notes what goes from it, for the host to take
  /// ([`Self::take_removed_renditions`], [`Self::take_media_purges`]). Without it nothing is noted.
  pub fn with_media_cache(mut self) -> Self {
    self.has_media_cache = true;
    self.with_conversation_purges()
  }

  /// The host keeps other files of conversations (the mobile apps' drafts): notes only which
  /// conversations went ([`Self::take_media_purges`]).
  pub fn with_conversation_purges(mut self) -> Self {
    self.notes_conversation_purges = true;
    self
  }

  /// The image attachments whose message went away since the last call.
  pub fn take_removed_renditions(&self) -> Vec<RemovedRendition> {
    let mut removed = self
      .removed_renditions
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    removed.drain(..).collect()
  }

  /// The conversations whose pictures are to go from the media cache (or the host's other files of
  /// a conversation), as a whole: purged or no longer listed. Each is handed out once until the
  /// host reports it with [`Self::media_purged`] or [`Self::media_purge_failed`]; the record stays
  /// until it is purged, also across a restart. Looks at the store only when something may wait.
  pub fn take_media_purges(&self) -> Vec<ConversationId> {
    if !self.notes_conversation_purges {
      return Vec::new();
    }
    let mut purges = self.purges();
    if !purges.is_due {
      return Vec::new();
    }
    let records = match self.inner.list_prefix(Table::SyncState, MEDIA_PURGE_PREFIX) {
      Ok(records) => records,
      Err(error) => {
        tracing::debug!(%error, "could not list the conversations to drop from the media cache");
        return Vec::new();
      },
    };
    let mut handed_out = Vec::new();
    let mut is_waiting = false;
    for conversation in records.iter().filter_map(|(key, _)| {
      key
        .strip_prefix(MEDIA_PURGE_PREFIX)?
        .parse::<ConversationId>()
        .ok()
    }) {
      if purges.in_flight.contains(&conversation) {
        continue;
      }
      if let Some(backoff) = purges.backoff.get_mut(&conversation)
        && backoff.skips_left > 0
      {
        backoff.skips_left -= 1;
        is_waiting = true;
        continue;
      }
      purges.in_flight.insert(conversation);
      handed_out.push(conversation);
    }
    // In flight ones make it due again when they fail.
    purges.is_due = is_waiting;
    handed_out
  }

  /// The host dropped `conversation`'s pictures: its record goes.
  pub fn media_purged(&self, conversation: ConversationId) -> Result<(), StoreError> {
    let deleted = self
      .inner
      .delete(Table::SyncState, &media_purge_key(conversation));
    let mut purges = self.purges();
    purges.in_flight.remove(&conversation);
    if deleted.is_ok() {
      purges.backoff.remove(&conversation);
    } else {
      purges.is_due = true;
    }
    deleted
  }

  /// The host couldn't drop `conversation`'s pictures now: handed out again later, after a pause
  /// that grows with each failure.
  pub fn media_purge_failed(&self, conversation: ConversationId) {
    let mut purges = self.purges();
    purges.in_flight.remove(&conversation);
    purges.is_due = true;
    let backoff = purges.backoff.entry(conversation).or_insert(Backoff {
      failures: 0,
      skips_left: 0,
    });
    backoff.failures = backoff.failures.saturating_add(1);
    backoff.skips_left = 1u32
      .checked_shl(backoff.failures)
      .unwrap_or(u32::MAX)
      .min(MAX_MEDIA_PURGE_SKIPS);
  }

  /// The wrapped store (for what only it offers, like flushing to disk).
  pub fn inner(&self) -> &S {
    &self.inner
  }

  /// Everything noted since the last call; starts noting afresh. Writes of another process reach
  /// no note, so a commit of one since the last call counts as a change of everything: the same
  /// look the store's caches and the message cache take, once per snapshot, and nothing more when
  /// no other process committed.
  pub fn take_changes(&self) -> StoreChanges {
    let is_foreign = self.take_foreign_commit();
    let mut changes = std::mem::take(&mut *self.lock());
    self.catch_up();
    if is_foreign {
      changes.is_foreign = true;
      changes.is_everything = true;
      changes.is_live = true;
      changes.is_security = true;
    }
    changes
  }

  /// Whether another process committed since the last call. A count, not a flag: the store's
  /// caches and the message cache look at it on their own.
  fn take_foreign_commit(&self) -> bool {
    self
      .inner
      .write_tally()
      .is_some_and(|tally| self.seen_foreign.swap(tally.foreign, Ordering::Relaxed) != tally.foreign)
  }

  fn note(&self, change: Change) {
    if self.is_numbered {
      let mut group = self.group();
      if group.is_mine() {
        group.changes.push(change);
        return;
      }
    }
    self.publish_change(change);
  }

  fn publish_change(&self, change: Change) {
    let mut changes = self.lock();
    match change {
      Change::Conversation(conversation) => {
        changes.conversations.insert(conversation);
      },
      Change::User(user) => {
        changes.users.insert(user);
      },
      Change::Announced(user) => {
        changes.announced.insert(user);
        // The person's entry shows the custom status the log carries.
        changes.users.insert(user);
        changes.is_live = true;
      },
      Change::Live => changes.is_live = true,
      Change::Security => changes.is_security = true,
      Change::Everything => changes.is_everything = true,
      Change::Workspace => {
        changes.is_security = true;
        changes.is_everything = true;
      },
      Change::Nothing => {},
    }
  }

  /// Notes the image attachments of `messages`, which are going away.
  fn note_removed(&self, messages: impl IntoIterator<Item = StoredMessage>) {
    if !self.has_media_cache {
      return;
    }
    let mut removed = self
      .removed_renditions
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    for message in messages {
      let Some(server_id) = message.maybe_server_id else {
        continue;
      };
      for attachment in attachments(&message.content) {
        let Ok(content_sha256) = <[u8; 32]>::try_from(attachment.ciphertext_sha256.as_slice()) else {
          continue;
        };
        if !attachment.media_type.starts_with("image/") {
          continue;
        }
        if removed.len() >= MAX_REMOVED_RENDITIONS {
          removed.pop_front();
        }
        removed.push_back(RemovedRendition {
          conversation: message.conversation_id,
          message: server_id,
          attachment: attachment.attachment_id,
          content_sha256,
        });
      }
    }
  }

  /// Every picture of `conversation`, which went away for this user: a record in the same group
  /// of writes, so it is not lost to a crash before the host dropped them.
  fn note_removed_conversation(&self, conversation: ConversationId) -> Result<(), StoreError> {
    if !self.notes_conversation_purges {
      return Ok(());
    }
    self
      .inner
      .put(Table::SyncState, &media_purge_key(conversation), &[])?;
    self.purges().is_due = true;
    Ok(())
  }

  /// Before the cache is read or patched: drops it if another process committed to the database.
  /// One `write_tally` look, which does not grow with the conversation (a `data_version` read).
  fn follow_disk(&self) {
    let maybe_tally = self.inner.write_tally();
    self.cache().follow_disk(maybe_tally);
    self.journal().follow_disk(maybe_tally);
  }

  /// Notes that a message (or a whole conversation) was written, before the write: inside a group
  /// over numbered commits, once the group ends.
  fn note_written(&self, conversation: ConversationId, written: Written, touches_files: bool) {
    if self.is_numbered {
      let mut group = self.group();
      if group.is_mine() {
        group.written.push((conversation, written, touches_files));
        group.conversations.insert(conversation);
        return;
      }
    }
    let mut journal = self.journal();
    journal.note(conversation, written);
    journal.attachments += u64::from(touches_files);
  }

  /// Runs `write` as a group of writes over numbered commits, so its notes are published after its
  /// commit; as it is otherwise.
  fn grouped<T>(&self, write: impl FnOnce() -> Result<T, StoreError>) -> Result<T, StoreError> {
    if self.is_numbered {
      super::write_group(self, write)
    } else {
      write()
    }
  }

  /// [`Self::grouped`] for a write of messages: natively always a group, so the search index's
  /// queue record (issue #154) reaches the disk with the message or not at all.
  fn message_group<T>(&self, write: impl FnOnce() -> Result<T, StoreError>) -> Result<T, StoreError> {
    if cfg!(target_arch = "wasm32") {
      self.grouped(write)
    } else {
      super::write_group(self, write)
    }
  }

  /// A reader takes a new snapshot if notes were published of commits its snapshot doesn't hold.
  /// Called after taking them: what was published before is covered by the number read here.
  fn catch_up(&self) {
    if let Some(seen) = self.inner.snapshot_commit()
      && seen < self.published.load(Ordering::SeqCst)
    {
      self.inner.renew_snapshot();
    }
  }

  /// Whether a reader may keep what it derives from `conversation` now: no open group wrote to it,
  /// and its snapshot holds every published commit.
  fn may_keep(&self, conversation: ConversationId) -> bool {
    let Some(seen) = self.inner.snapshot_commit() else {
      return true;
    };
    !self.group().conversations.contains(&conversation) && seen >= self.published.load(Ordering::SeqCst)
  }

  /// The group of writes ends: its notes are published, after its commit's number.
  fn publish(&self, notes: GroupNotes) {
    if let Some(committed) = self.inner.last_commit() {
      self.published.fetch_max(committed, Ordering::SeqCst);
    }
    if !notes.written.is_empty() {
      let mut journal = self.journal();
      for (conversation, written, touches_files) in notes.written {
        journal.note(conversation, written);
        journal.attachments += u64::from(touches_files);
      }
    }
    for change in notes.changes {
      self.publish_change(change);
    }
  }

  fn delete_grouped(&self, table: Table, key: &str) -> Result<(), StoreError> {
    if table == Table::Conversations
      && let Ok(conversation) = key.parse::<ConversationId>()
    {
      self.note_removed_conversation(conversation)?;
    }
    if table == Table::Conversations {
      self.inner.delete(Table::EntryTallies, key)?;
      if let Ok(conversation) = key.parse::<ConversationId>() {
        self
          .inner
          .delete(Table::Account, &draft_reply_key(conversation))?;
      }
    }
    self.inner.delete(table, key)?;
    self.note(classify(table, key));
    Ok(())
  }

  fn upsert_message_grouped(&self, message: &StoredMessage) -> Result<(), StoreError> {
    // Only a deletion takes files away (an edit changes the text); only then is the old one read.
    let mut touches_files = !attachments(&message.content).is_empty();
    if matches!(message.content, MessageContent::Deleted)
      && let Some(old) = self
        .inner
        .message(message.conversation_id, message.client_message_id)?
    {
      touches_files |= !attachments(&old.content).is_empty();
      self.note_removed([old]);
    }
    self.note_written(
      message.conversation_id,
      Written::Message(message.client_message_id),
      touches_files,
    );
    self.inner.upsert_message(message)?;
    // Search covers it, or may have covered it before (an edit, a deletion; issue #154).
    #[cfg(not(target_arch = "wasm32"))]
    if matches!(
      message.content,
      MessageContent::Text { .. } | MessageContent::File { .. } | MessageContent::Deleted
    ) {
      crate::search_index::queue_message(&self.inner, message.conversation_id, message.client_message_id)?;
    }
    // After the write: a commit of another process before it is counted by now.
    self.follow_disk();
    self.cache().upsert(message);
    self.note(Change::Conversation(message.conversation_id));
    Ok(())
  }

  fn delete_messages_grouped(&self, conversation: ConversationId) -> Result<(), StoreError> {
    // Not read message by message: a big channel would be held in memory whole.
    self.note_removed_conversation(conversation)?;
    self.note_written(conversation, Written::Conversation, true);
    self.inner.delete_messages(conversation)?;
    #[cfg(not(target_arch = "wasm32"))]
    crate::search_index::queue_conversation(&self.inner, conversation)?;
    self
      .inner
      .delete(Table::EntryTallies, &conversation.to_string())?;
    // After the write: a commit of another process before it is counted by now.
    self.follow_disk();
    self.cache().remove_conversation(conversation);
    self.note(Change::Conversation(conversation));
    Ok(())
  }

  fn delete_message_grouped(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<(), StoreError> {
    let mut touches_files = false;
    if let Some(old) = self.inner.message(conversation, client_message_id)? {
      touches_files = !attachments(&old.content).is_empty();
      self.note_removed([old]);
    }
    self.note_written(conversation, Written::Message(client_message_id), touches_files);
    self.inner.delete_message(conversation, client_message_id)?;
    #[cfg(not(target_arch = "wasm32"))]
    crate::search_index::queue_message(&self.inner, conversation, client_message_id)?;
    // After the write: a commit of another process before it is counted by now.
    self.follow_disk();
    self.cache().remove(conversation, client_message_id);
    self.note(Change::Conversation(conversation));
    Ok(())
  }

  fn group(&self) -> std::sync::MutexGuard<'_, GroupNotes> {
    // Invariant-free data: at worst a note is published early or a reader reads again.
    self.group.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn journal(&self) -> std::sync::MutexGuard<'_, Journal> {
    // Invariant-free data: at worst a reader reads again what it keeps.
    self.journal.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn cache(&self) -> std::sync::MutexGuard<'_, MessageCache> {
    // Invariant-free data: at worst a conversation is read from the disk again.
    self.messages.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn purges(&self) -> std::sync::MutexGuard<'_, MediaPurges> {
    // Invariant-free data: at worst a purge is handed out again (dropping is idempotent).
    self.media_purges.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, StoreChanges> {
    // Invariant-free data: a panic elsewhere leaves a usable (at worst incomplete) set of notes.
    self.changes.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

impl<S: LocalStore> LocalStore for TrackedStore<S> {
  fn get(&self, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    self.inner.get(table, key)
  }

  /// Rewriting a record the snapshot reads with the same bytes (a member list fetched again, say)
  /// is neither written nor noted.
  fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError> {
    let change = classify(table, key);
    // Compared inside the group: a reading connection's snapshot may be older than the record.
    self.grouped(|| {
      if change != Change::Nothing && self.inner.get(table, key)?.as_deref() == Some(value) {
        return Ok(());
      }
      self.inner.put(table, key, value)?;
      self.note(change);
      Ok(())
    })
  }

  fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
    self.grouped(|| self.delete_grouped(table, key))
  }

  fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    self.inner.list(table)
  }

  fn list_prefix(&self, table: Table, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    self.inner.list_prefix(table, prefix)
  }

  fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
    self.message_group(|| self.upsert_message_grouped(message))
  }

  fn message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    self.inner.message(conversation, client_message_id)
  }

  fn message_by_server_id(
    &self,
    conversation: ConversationId,
    server_id: MessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    self.inner.message_by_server_id(conversation, server_id)
  }

  fn edge_server_id(
    &self,
    conversation: ConversationId,
    edge: MessageEdge,
  ) -> Result<Option<MessageId>, StoreError> {
    self.inner.edge_server_id(conversation, edge)
  }

  /// From the cache when it holds the conversation; otherwise read whole and kept, unless a write
  /// to it came in between (the read may predate it).
  fn messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    if !self.cache().is_enabled() {
      return self.inner.messages(conversation, filter);
    }
    let mut matching = Vec::new();
    self.with_all_messages(conversation, &mut |messages| {
      matching = messages
        .iter()
        .filter(|message| message.matches(filter))
        .cloned()
        .collect();
    })?;
    Ok(matching)
  }

  fn messages_at(
    &self,
    conversation: ConversationId,
    slot: MessageSlot,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.inner.messages_at(conversation, slot)
  }

  fn message_revision(&self, conversation: ConversationId) -> Option<u64> {
    self.follow_disk();
    self.catch_up();
    let revision = self.cache().revision(conversation);
    // Checked after the revision is read: a write that moved it is still open, or published.
    self.may_keep(conversation).then_some(revision)
  }

  fn visit_messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
    visit: &mut dyn FnMut(&StoredMessage),
  ) -> Result<(), StoreError> {
    self.with_all_messages(conversation, &mut |messages| {
      for message in messages.iter().filter(|message| message.matches(filter)) {
        visit(message);
      }
    })
  }

  /// Without copies when the cache holds the conversation (the cache's lock is not held while
  /// `read` runs); otherwise read from the store and kept, as by [`Self::messages`].
  fn with_all_messages(
    &self,
    conversation: ConversationId,
    read: &mut dyn FnMut(&[StoredMessage]),
  ) -> Result<(), StoreError> {
    self.follow_disk();
    let revision = {
      let mut cache = self.cache();
      if let Some(messages) = cache.shared(conversation) {
        drop(cache);
        read(&messages);
        return Ok(());
      }
      if !cache.is_enabled() {
        drop(cache);
        read(&self.inner.messages(conversation, MessageFilter::All)?);
        return Ok(());
      }
      cache.revision(conversation)
    };
    self.catch_up();
    let all = self.inner.messages(conversation, MessageFilter::All)?;
    read(&all);
    if self.may_keep(conversation) {
      self.cache().fill(conversation, revision, all);
    }
    Ok(())
  }

  fn attached_messages(
    &self,
    conversation: ConversationId,
    kind: Attached,
    range: AttachedRange,
    limit: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.inner.attached_messages(conversation, kind, range, limit)
  }

  fn indexes_feed(&self) -> bool {
    self.inner.indexes_feed()
  }

  fn feed_len(&self, conversation: ConversationId) -> Result<usize, StoreError> {
    self.inner.feed_len(conversation)
  }

  fn feed_rows(
    &self,
    conversation: ConversationId,
    start: usize,
    len: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.inner.feed_rows(conversation, start, len)
  }

  fn feed_ranks(
    &self,
    conversation: ConversationId,
    orders: &[MessageOrder],
  ) -> Result<Vec<usize>, StoreError> {
    self.inner.feed_ranks(conversation, orders)
  }

  fn event_messages(&self, conversation: ConversationId) -> Result<Vec<StoredMessage>, StoreError> {
    self.inner.event_messages(conversation)
  }

  fn reactions_to(
    &self,
    conversation: ConversationId,
    targets: &[ClientMessageId],
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.inner.reactions_to(conversation, targets)
  }

  fn reply_counts(
    &self,
    conversation: ConversationId,
    threads: &[ThreadId],
  ) -> Result<Vec<usize>, StoreError> {
    self.inner.reply_counts(conversation, threads)
  }

  fn feed_version(&self, conversation: ConversationId) -> Result<Option<FeedVersion>, StoreError> {
    self.inner.feed_version(conversation)
  }

  fn written_cursor(&self) -> Option<u64> {
    let next = self.journal().next;
    self.catch_up();
    Some(next)
  }

  fn attachments_cursor(&self) -> Option<u64> {
    // Another connection's commit counts as a change of files: it doesn't say what it wrote.
    self.follow_disk();
    Some(self.journal().attachments)
  }

  fn is_awaited(&self) -> bool {
    self.inner.is_awaited()
  }

  fn begin_reads(&self) -> Result<(), StoreError> {
    self.inner.begin_reads()
  }

  fn end_reads(&self) {
    self.inner.end_reads();
  }

  fn written_since(&self, conversation: ConversationId, cursor: u64) -> Option<Vec<Written>> {
    // Another connection's commit since the last write is found now.
    self.follow_disk();
    let maybe_written = self.journal().since(conversation, cursor);
    self.catch_up();
    maybe_written
  }

  fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
    self.message_group(|| self.delete_messages_grouped(conversation))
  }

  fn delete_message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<(), StoreError> {
    self.message_group(|| self.delete_message_grouped(conversation, client_message_id))
  }

  fn purge_freed(&self) -> Result<(), StoreError> {
    self.inner.purge_freed()
  }

  fn purge_freed_soon(&self) {
    self.inner.purge_freed_soon();
  }

  fn wait_for_purge(&self, max_wait: std::time::Duration) -> bool {
    self.inner.wait_for_purge(max_wait)
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    // The store's group is this thread's from here on: no other thread writes until it ends.
    self.inner.begin_writes()?;
    if self.is_numbered {
      let mut group = self.group();
      group.maybe_owner = Some(thread::current().id());
      group.depth += 1;
    }
    Ok(())
  }

  fn end_writes(&self) -> Result<(), StoreError> {
    // Taken before the store's group ends: the next thread's group may begin right after it.
    let maybe_notes = self.is_numbered.then(|| self.group().end()).flatten();
    // The group may be rolled back: the cache may then hold writes the disk doesn't. That holds for
    // a panic out of the inner store too (its catalog flush rolls the group back and resumes the
    // panic), which skips everything after the call; the guard forgets the cache on the way out
    // (issue #1201). The cache doesn't follow lost writes by itself (`MessageCache::follow_disk`),
    // so a client reopened on this store after the panic would read the rolled-back messages.
    let mut guard = ForgetOnFailure {
      messages: &self.messages,
      is_armed: true,
    };
    let ended = self.inner.end_writes();
    guard.is_armed = ended.is_err();
    // Published also after a rollback: a reader then reads again what didn't change.
    if let Some(notes) = maybe_notes {
      self.publish(notes);
    }
    ended
  }

  fn fail_writes(&self) {
    self.inner.fail_writes();
  }

  fn write_soon(&self) {
    self.inner.write_soon();
  }

  fn write_tally(&self) -> Option<WriteTally> {
    self.inner.write_tally()
  }

  fn lost_deletes(&self, table: Table) -> Vec<String> {
    self.inner.lost_deletes(table)
  }
}

/// Forgets the message cache when dropped while armed: [`TrackedStore::end_writes`] disarms it
/// once the group committed, and an error or a panic leaves it armed. A drop guard rather than
/// `catch_unwind`, which the browser build (panics abort there) can't use anyway.
struct ForgetOnFailure<'a> {
  messages: &'a Mutex<MessageCache>,
  is_armed: bool,
}

impl Drop for ForgetOnFailure<'_> {
  fn drop(&mut self) {
    if self.is_armed {
      // Invariant-free data, as in `TrackedStore::cache`.
      self
        .messages
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .forget_all();
    }
  }
}

/// What a write to `table` under `key` may change in the snapshot.
/// The files a message carries.
fn attachments(content: &MessageContent) -> &[AttachmentRef] {
  match content {
    MessageContent::File { attachment } => std::slice::from_ref(attachment),
    MessageContent::Text { meta, .. } => &meta.attachments,
    _ => &[],
  }
}

fn media_purge_key(conversation: ConversationId) -> String {
  format!("{MEDIA_PURGE_PREFIX}{conversation}")
}

fn classify(table: Table, key: &str) -> Change {
  match table {
    Table::Spaces | Table::Conversations | Table::Users | Table::Categories => Change::Everything,
    // The space settings read them; written when they open and after each change.
    Table::SpaceRosters | Table::SpaceInvites => Change::Everything,
    Table::Workspace => Change::Workspace,
    Table::Members => conversation_of(key),
    // Keyed `conversation/…`: shown with the conversation's messages.
    Table::MessageAcks | Table::Outbox | Table::PendingUploads | Table::PendingPreviews => {
      conversation_of(key)
    },
    Table::Account if SHARED_ACCOUNT_RECORDS.contains(&key) => Change::Everything,
    // A conversation's bookmarks bar and its shared page (e2ee.md §4.11).
    Table::Account => key
      .strip_prefix(BOOKMARKS_PREFIX)
      .or_else(|| key.strip_prefix(crate::canvas::CANVAS_PREFIX))
      .map_or(Change::Nothing, conversation_of),
    // The presence log's head, a person's state, or their last seen (read by queries only).
    Table::Presence if key == crate::read_state::ANNOUNCED_PRESENCE_KEY => Change::Live,
    Table::Presence => key.parse().map_or(Change::Nothing, Change::User),
    Table::AnnouncedPresence => key.parse().map_or(Change::Live, Change::Announced),
    Table::Unread | Table::Threads | Table::ReadReceipts => Change::Live,
    Table::PinnedIdentities => Change::Security,
    // The "catching up" marks and the unseen reactions; the rest are sync cursors.
    Table::SyncState => key
      .strip_prefix(crate::client::CATCHING_UP_PREFIX)
      .or_else(|| key.strip_prefix(crate::unseen_reactions::INDEX_PREFIX))
      .map_or(Change::Nothing, conversation_of),
    Table::ContentKeys
    | Table::MlsStorage
    | Table::AppliedChanges
    | Table::Undecrypted
    | Table::PendingChanges
    | Table::SearchSegments
    | Table::SegmentAnnouncements
    | Table::SavedSearches
    | Table::FileTransfers
    | Table::Downloads
    | Table::EntryTallies
    | Table::SearchIndex
    | Table::SearchQueue
    | Table::ShareLinks
    | Table::ChannelBrowser
    | Table::DeviceLists
    | Table::DeviceKeys
    | Table::RemovedSenders
    // Queued actions are applied to the shown state when queued; the queue itself is not shown.
    | Table::PendingActions => Change::Nothing,
  }
}

/// The conversation a key starts with (`conversation` or `conversation/…`).
fn conversation_of(key: &str) -> Change {
  let head = key.split_once('/').map_or(key, |(head, _)| head);
  head.parse().map_or(Change::Nothing, Change::Conversation)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;
  use crate::store::MemoryStore;
  use crate::store::tests::exercise_store;

  #[test]
  fn it_is_a_store() {
    exercise_store(&TrackedStore::new(MemoryStore::new()));
  }

  #[test]
  fn writes_are_noted_by_what_they_change() {
    let store = TrackedStore::new(MemoryStore::new());
    store.put(Table::Members, "7", b"[]").unwrap();
    store.put(Table::Account, "bookmarks/8", b"[]").unwrap();
    store.put(Table::SyncState, "catching_up/9", b"true").unwrap();
    store.put(Table::Presence, "3", b"\"online\"").unwrap();
    store.put(Table::Presence, "announced", b"{}").unwrap();
    store.put(Table::AnnouncedPresence, "4", b"{}").unwrap();
    store.put(Table::MlsStorage, "x", b"1").unwrap();
    assert_eq!(
      store.take_changes(),
      StoreChanges {
        conversations: [id(7), id(8), id(9)].into(),
        users: [id(3), id(4)].into(),
        announced: [id(4)].into(),
        is_live: true,
        ..StoreChanges::default()
      }
    );
    assert_eq!(store.take_changes(), StoreChanges::default(), "taken");

    store.put(Table::Users, "3", b"{}").unwrap();
    store.put(Table::Workspace, "state", b"{}").unwrap();
    let changes = store.take_changes();
    assert!(changes.is_everything && changes.is_security);
  }

  #[test]
  fn the_same_bytes_again_are_no_change() {
    let store = TrackedStore::new(MemoryStore::new());
    store.put(Table::Members, "7", b"[1]").unwrap();
    store.take_changes();
    store.put(Table::Members, "7", b"[1]").unwrap();
    assert_eq!(store.take_changes(), StoreChanges::default());
    store.put(Table::Members, "7", b"[2]").unwrap();
    assert_eq!(store.take_changes().conversations, [id(7)].into());
  }

  #[test]
  fn records_the_snapshot_does_not_read_change_nothing() {
    let store = TrackedStore::new(MemoryStore::new());
    store.put(Table::Presence, "last_seen:3", b"{}").unwrap();
    store.put(Table::Account, "push_session", b"{}").unwrap();
    store.put(Table::SyncState, "sync", b"{}").unwrap();
    assert_eq!(store.take_changes(), StoreChanges::default());
  }

  mod removed_renditions {
    use base64::Engine;

    use super::*;

    #[test]
    fn a_deleted_message_gives_up_its_pictures() {
      let store = TrackedStore::new(MemoryStore::new()).with_media_cache();
      let picture = file_message(1, Some(10), "image/png");
      store.upsert_message(&picture).unwrap();
      store
        .upsert_message(&file_message(2, Some(11), "application/pdf"))
        .unwrap();
      assert!(
        store.take_removed_renditions().is_empty(),
        "nothing went away yet"
      );
      store
        .upsert_message(&StoredMessage {
          content: MessageContent::Deleted,
          ..picture
        })
        .unwrap();
      assert_eq!(store.take_removed_renditions(), vec![removed(10)]);
      assert!(store.take_removed_renditions().is_empty(), "taken");
    }

    #[test]
    fn a_message_or_a_conversation_that_goes_away_gives_up_its_pictures() {
      let store = TrackedStore::new(MemoryStore::new()).with_media_cache();
      store
        .upsert_message(&file_message(1, Some(10), "image/png"))
        .unwrap();
      store
        .upsert_message(&file_message(2, Some(11), "image/jpeg"))
        .unwrap();
      // An outbox message has no rendition in the cache (it has no server ID yet).
      store.upsert_message(&file_message(3, None, "image/png")).unwrap();
      store
        .delete_message(id(7), ClientMessageId::from_bytes([1; 16]))
        .unwrap();
      assert_eq!(store.take_removed_renditions(), vec![removed(10)]);
      assert!(store.take_media_purges().is_empty());
      store.put(Table::Conversations, "7", b"{}").unwrap();
      store.delete(Table::Conversations, "7").unwrap();
      assert_eq!(
        store.take_media_purges(),
        vec![id(7)],
        "no longer listed: all of it goes, sent or not"
      );
      assert!(store.take_removed_renditions().is_empty(), "not one by one");
    }

    #[test]
    fn notes_nobody_takes_stay_bounded() {
      let store = TrackedStore::new(MemoryStore::new()).with_media_cache();
      let extra = 10;
      store.note_removed(
        (0..MAX_REMOVED_RENDITIONS + extra).map(|n| file_message(1, Some(1_000 + n as i64), "image/png")),
      );
      let kept = store.take_removed_renditions();
      assert_eq!(kept.len(), MAX_REMOVED_RENDITIONS);
      assert_eq!(kept[0].message, id(1_000 + extra as i64), "the oldest went");
    }

    #[test]
    fn a_purge_notes_the_conversation_however_big_it_is() {
      let store = TrackedStore::new(MemoryStore::new()).with_media_cache();
      let many = MAX_REMOVED_RENDITIONS + 10;
      for n in 0..many {
        let mut message = file_message(1, Some(1_000 + n as i64), "image/png");
        message.client_message_id = ClientMessageId::from_bytes((n as u128).to_be_bytes());
        store.upsert_message(&message).unwrap();
      }
      store.delete_messages(id(7)).unwrap();
      assert_eq!(store.take_media_purges(), vec![id(7)]);
      assert!(store.take_removed_renditions().is_empty());
    }

    #[test]
    fn a_purge_stays_due_until_the_host_confirms_it_also_across_a_restart() {
      let disk = Arc::new(MemoryStore::new());
      let store = TrackedStore::new(disk.clone()).with_media_cache();
      store.begin_writes().unwrap();
      store.delete_messages(id(7)).unwrap();
      store.delete_messages(id(8)).unwrap();
      store.end_writes().unwrap();
      assert_eq!(store.take_media_purges(), vec![id(7), id(8)]);
      assert!(store.take_media_purges().is_empty(), "in flight: once");
      store.media_purged(id(7)).unwrap();
      // The app ended before the cache dropped the second one.
      drop(store);

      let restarted = TrackedStore::new(disk).with_media_cache();
      assert_eq!(restarted.take_media_purges(), vec![id(8)]);
      restarted.media_purged(id(8)).unwrap();
      assert!(restarted.take_media_purges().is_empty());
      assert!(
        restarted.take_media_purges().is_empty(),
        "and the store isn't read again"
      );
    }

    #[test]
    fn a_failed_purge_comes_back_after_a_pause_that_grows() {
      let store = TrackedStore::new(MemoryStore::new()).with_media_cache();
      store.delete_messages(id(7)).unwrap();
      assert_eq!(store.take_media_purges(), vec![id(7)]);
      store.media_purge_failed(id(7));
      assert_eq!(looks_until_handed_out(&store), 2);
      store.media_purge_failed(id(7));
      assert_eq!(looks_until_handed_out(&store), 4);
      for _ in 0..10 {
        store.media_purge_failed(id(7));
        looks_until_handed_out(&store);
      }
      store.media_purge_failed(id(7));
      assert_eq!(
        looks_until_handed_out(&store),
        MAX_MEDIA_PURGE_SKIPS as usize,
        "capped"
      );
      store.media_purged(id(7)).unwrap();
      assert!(store.take_media_purges().is_empty());
    }

    #[test]
    fn without_a_media_cache_nothing_is_noted() {
      let store = TrackedStore::new(MemoryStore::new());
      let picture = file_message(1, Some(10), "image/png");
      store.upsert_message(&picture).unwrap();
      store
        .upsert_message(&StoredMessage {
          content: MessageContent::Deleted,
          ..picture
        })
        .unwrap();
      store.delete_messages(id(7)).unwrap();
      assert!(store.take_removed_renditions().is_empty());
      assert!(store.take_media_purges().is_empty());
      assert!(
        store
          .list_prefix(Table::SyncState, MEDIA_PURGE_PREFIX)
          .unwrap()
          .is_empty(),
        "no record that nobody would delete"
      );
    }

    #[test]
    fn the_attachments_cursor_moves_only_with_files() {
      let store = TrackedStore::new(MemoryStore::new());
      let cursor = || store.attachments_cursor().unwrap();
      let start = cursor();
      let mut text = file_message(1, Some(10), "image/png");
      text.content = MessageContent::Text {
        body: "hello".into(),
        mentions: Vec::new(),
        meta: Default::default(),
      };
      store.upsert_message(&text).unwrap();
      assert_eq!(cursor(), start, "a text without files");
      store
        .upsert_message(&StoredMessage {
          content: MessageContent::Deleted,
          ..text
        })
        .unwrap();
      assert_eq!(cursor(), start, "deleting a text without files");
      let document = file_message(2, Some(20), "application/pdf");
      store.upsert_message(&document).unwrap();
      let added = cursor();
      assert!(added > start, "a file came");
      store
        .upsert_message(&StoredMessage {
          content: MessageContent::Deleted,
          ..document.clone()
        })
        .unwrap();
      let deleted = cursor();
      assert!(deleted > added, "deleted for everyone");
      store
        .upsert_message(&file_message(3, Some(30), "image/png"))
        .unwrap();
      let again = cursor();
      store
        .delete_message(id(7), ClientMessageId::from_bytes([3; 16]))
        .unwrap();
      assert!(cursor() > again, "removed from the store");
      let before_purge = cursor();
      store.delete_messages(id(7)).unwrap();
      assert!(cursor() > before_purge, "a conversation that went");
    }

    /// How many looks pass until the failed purge of conversation 7 is handed out again.
    fn looks_until_handed_out(store: &TrackedStore<MemoryStore>) -> usize {
      (0..1_000)
        .find(|_| store.take_media_purges() == vec![id(7)])
        .expect("handed out again")
    }

    fn removed(message: i64) -> RemovedRendition {
      RemovedRendition {
        conversation: id(7),
        message: id(message),
        attachment: id(message),
        content_sha256: [9; 32],
      }
    }

    fn file_message(client: u8, maybe_server: Option<i64>, media_type: &str) -> StoredMessage {
      let attachment = serde_json::json!({
        "attachment_id": id::<AttachmentId>(maybe_server.unwrap_or(1)),
        "key": "",
        "ciphertext_sha256": base64::engine::general_purpose::STANDARD.encode([9u8; 32]),
        "file_name": "f",
        "media_type": media_type,
        "size_bytes": 1,
      });
      StoredMessage {
        conversation_id: id(7),
        client_message_id: ClientMessageId::from_bytes([client; 16]),
        maybe_server_id: maybe_server.map(id),
        sender_user_id: id(1),
        maybe_thread_id: None,
        sent_at_ms: maybe_server.unwrap_or(0),
        content: MessageContent::File {
          attachment: serde_json::from_value(attachment).unwrap(),
        },
        is_edited: false,
        edits: Vec::new(),
        maybe_edit_id: None,
        is_pinned: false,
        maybe_verified_commit: None,
        maybe_server_change: None,
        maybe_unreadable: None,
      }
    }
  }

  mod message_cache {
    use super::*;

    /// Room for every message these tests write.
    const ROOM_BYTES: usize = 1 << 20;

    #[test]
    fn it_is_a_store_with_a_cache() {
      exercise_store(&TrackedStore::with_message_cache(MemoryStore::new(), ROOM_BYTES));
    }

    #[test]
    fn reads_after_writes_through_it_see_them_in_order() {
      let store = TrackedStore::with_message_cache(MemoryStore::new(), ROOM_BYTES);
      store.upsert_message(&text(1, Some(300), None)).unwrap();
      store.upsert_message(&text(2, Some(100), None)).unwrap();
      assert_eq!(clients(&store, MessageFilter::All), [2, 1], "read and kept");

      store.upsert_message(&text(3, Some(200), None)).unwrap();
      store.upsert_message(&text(4, None, None)).unwrap();
      store.upsert_message(&text(5, Some(400), Some(100))).unwrap();
      assert_eq!(clients(&store, MessageFilter::All), [2, 3, 1, 5, 4]);
      assert_eq!(clients(&store, MessageFilter::TopLevel), [2, 3, 1, 4]);
      assert_eq!(clients(&store, MessageFilter::Thread(id(100))), [5]);

      // An outbox message that was sent moves to its place.
      store.upsert_message(&text(4, Some(250), None)).unwrap();
      store
        .delete_message(id(7), ClientMessageId::from_bytes([2; 16]))
        .unwrap();
      assert_eq!(clients(&store, MessageFilter::All), [3, 4, 1, 5]);
      assert_eq!(
        clients(&store, MessageFilter::All),
        clients_of(store.inner().messages(id(7), MessageFilter::All).unwrap()),
        "the cache says what the store says"
      );
    }

    #[test]
    fn a_conversation_larger_than_the_cache_is_read_from_the_store_each_time() {
      let store =
        TrackedStore::with_message_cache(MemoryStore::new(), 2 * text(1, None, None).footprint_bytes());
      store.upsert_message(&text(1, Some(100), None)).unwrap();
      store.upsert_message(&text(2, Some(200), None)).unwrap();
      assert_eq!(clients(&store, MessageFilter::All), [1, 2]);
      store.upsert_message(&text(3, Some(300), None)).unwrap();
      assert_eq!(clients(&store, MessageFilter::All), [1, 2, 3]);
    }

    #[test]
    fn the_revision_moves_with_each_write_to_the_conversation_only() {
      let store = TrackedStore::with_message_cache(MemoryStore::new(), ROOM_BYTES);
      let before = store.message_revision(id(7));
      store.upsert_message(&text(1, Some(100), None)).unwrap();
      let after = store.message_revision(id(7));
      assert_ne!(before, after);
      store.put(Table::Members, "7", b"[]").unwrap();
      assert_eq!(store.message_revision(id(7)), after);
      assert_eq!(store.message_revision(id(8)), before);
    }

    /// Issue #1124: another process (the iOS extension) committed to the same file; the cache
    /// must not keep serving the conversation as it was.
    #[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
    mod other_connections {
      use teamcraft_crypto::secret::SymmetricKey;

      use super::*;
      use crate::store::{SealedSqliteStore, StaticKeySource};

      #[test]
      fn another_connections_commit_drops_the_cached_conversation() {
        let (directory, ours, theirs) = two_connections();
        ours.upsert_message(&text(1, Some(100), None)).unwrap();
        ours.upsert_message(&text(2, Some(200), None)).unwrap();
        let before = ours.message_revision(id(7));
        assert_eq!(clients(&ours, MessageFilter::All), [1, 2], "read and kept");

        theirs.upsert_message(&text(3, Some(300), None)).unwrap();
        theirs
          .delete_message(id(7), ClientMessageId::from_bytes([1; 16]))
          .unwrap();

        assert_ne!(ours.message_revision(id(7)), before, "derived data is stale too");
        assert_eq!(clients(&ours, MessageFilter::All), [2, 3]);
        drop((ours, theirs));
        std::fs::remove_dir_all(&directory).unwrap();
      }

      #[test]
      fn a_write_after_another_connections_commit_does_not_patch_a_stale_conversation() {
        let (directory, ours, theirs) = two_connections();
        ours.upsert_message(&text(1, Some(100), None)).unwrap();
        assert_eq!(clients(&ours, MessageFilter::All), [1]);

        theirs.upsert_message(&text(2, Some(200), None)).unwrap();
        ours.upsert_message(&text(3, Some(300), None)).unwrap();

        assert_eq!(clients(&ours, MessageFilter::All), [1, 2, 3]);
        drop((ours, theirs));
        std::fs::remove_dir_all(&directory).unwrap();
      }

      #[test]
      fn our_own_commits_keep_the_cache() {
        let (directory, ours, theirs) = two_connections();
        ours.upsert_message(&text(1, Some(100), None)).unwrap();
        assert_eq!(clients(&ours, MessageFilter::All), [1]);
        let revision = ours.message_revision(id(7));
        // A drop moves the revision, so it stays while the cache is kept.
        assert_eq!(ours.message_revision(id(7)), revision);
        assert_eq!(clients(&ours, MessageFilter::All), [1]);
        assert_eq!(ours.message_revision(id(7)), revision);
        drop((ours, theirs));
        std::fs::remove_dir_all(&directory).unwrap();
      }

      /// Issue #1115: what another process wrote reaches no note, so its commit is read as a change
      /// of everything, once.
      #[test]
      fn another_connections_commit_counts_as_a_change_of_everything_once() {
        let (directory, ours, theirs) = two_connections();
        ours.put(Table::Users, "9", b"mine").unwrap();
        let own = ours.take_changes();
        assert!(
          own.is_everything && !own.is_foreign,
          "our commits are noted as they are"
        );

        theirs.put(Table::Account, "bookkeeping", b"seen").unwrap();
        let changed = ours.take_changes();
        assert!(changed.is_foreign && changed.is_everything && changed.is_live && changed.is_security);

        assert_eq!(
          ours.take_changes(),
          StoreChanges::default(),
          "no other commit, no change"
        );
        drop((ours, theirs));
        std::fs::remove_dir_all(&directory).unwrap();
      }

      fn two_connections() -> (
        std::path::PathBuf,
        TrackedStore<SealedSqliteStore>,
        SealedSqliteStore,
      ) {
        let directory = std::env::temp_dir().join(format!(
          "teamcraft-tracked-{}",
          rand_core::RngCore::next_u64(&mut rand_core::OsRng)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("store.db");
        let key = SymmetricKey::generate(&mut rand_core::OsRng);
        let ours = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
        let theirs = SealedSqliteStore::open(&path, &StaticKeySource::new(key)).unwrap();
        (
          directory,
          TrackedStore::with_message_cache(ours, ROOM_BYTES),
          theirs,
        )
      }

      fn clients(store: &TrackedStore<SealedSqliteStore>, filter: MessageFilter) -> Vec<u8> {
        store
          .messages(id(7), filter)
          .unwrap()
          .iter()
          .map(|message| message.client_message_id.as_bytes()[0])
          .collect()
      }
    }

    fn text(client: u8, maybe_server: Option<i64>, maybe_thread: Option<i64>) -> StoredMessage {
      StoredMessage {
        conversation_id: id(7),
        client_message_id: ClientMessageId::from_bytes([client; 16]),
        maybe_server_id: maybe_server.map(id),
        sender_user_id: id(1),
        maybe_thread_id: maybe_thread.map(id),
        sent_at_ms: i64::from(client),
        content: MessageContent::Text {
          body: format!("message {client}"),
          mentions: Vec::new(),
          meta: Default::default(),
        },
        is_edited: false,
        edits: Vec::new(),
        maybe_edit_id: None,
        is_pinned: false,
        maybe_verified_commit: None,
        maybe_server_change: None,
        maybe_unreadable: None,
      }
    }

    fn clients(store: &TrackedStore<MemoryStore>, filter: MessageFilter) -> Vec<u8> {
      clients_of(store.messages(id(7), filter).unwrap())
    }

    fn clients_of(messages: Vec<StoredMessage>) -> Vec<u8> {
      messages
        .iter()
        .map(|message| message.client_message_id.as_bytes()[0])
        .collect()
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}

#[cfg(test)]
mod journal_tests {
  use super::*;

  fn conversation(raw: i64) -> ConversationId {
    <ConversationId as teamcraft_proto::Id>::from_snowflake(teamcraft_proto::Snowflake::new(raw).unwrap())
  }

  fn written(n: u8) -> Written {
    Written::Message(ClientMessageId::from_bytes([n; 16]))
  }

  #[test]
  fn tells_what_came_after_a_cursor_in_one_conversation() {
    let mut journal = Journal::default();
    journal.note(conversation(1), written(1));
    let cursor = journal.next;
    journal.note(conversation(1), written(2));
    journal.note(conversation(2), written(3));
    assert_eq!(journal.since(conversation(1), cursor), Some(vec![written(2)]));
    assert_eq!(journal.since(conversation(2), journal.next), Some(Vec::new()));
  }

  #[test]
  fn cannot_tell_past_what_it_dropped_or_another_connection_wrote() {
    let mut journal = Journal::default();
    let cursor = journal.next;
    for n in 0..=Journal::MAX_ENTRIES {
      journal.note(conversation(1), written((n % 200) as u8));
    }
    assert_eq!(journal.since(conversation(1), cursor), None);
    let fresh = journal.next;
    journal.follow_disk(Some(WriteTally { lost: 0, foreign: 1 }));
    assert_eq!(journal.since(conversation(1), fresh - 1), None);
    assert_eq!(journal.since(conversation(1), journal.next), Some(Vec::new()));
    assert_eq!(
      journal.since(conversation(1), journal.next + 1),
      None,
      "a cursor from the future"
    );
  }
}
