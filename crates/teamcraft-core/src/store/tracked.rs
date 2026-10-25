//! A store that notes which parts of the UI's snapshot its writes may have changed, so the snapshot
//! is rebuilt per conversation instead of reading the whole workspace on every change (issue #246).
//!
//! The engine and the UI adapter share one [`TrackedStore`]; the host takes the [`StoreChanges`]
//! when the UI asks what changed ([`crate::ui_snapshot::SnapshotCache`]). Writes are classified by
//! table and key: what one conversation's sidebar entry or feed reads, what one person's entry
//! reads, the live state (counts, threads, presence log, receipts), the security part, or what
//! every conversation may depend on (names, spaces, preferences). Tables the snapshot doesn't read
//! (MLS state, sync cursors, search segments, …) are not noted at all.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use teamcraft_proto::{ClientMessageId, ConversationId, MessageId, UserId};

use super::{LocalStore, MessageFilter, StoreError, StoredMessage, Table};

/// Records of [`Table::Account`] every conversation entry reads (unread marks, mention rules).
const SHARED_ACCOUNT_RECORDS: [&str; 2] = [
  crate::client::ACCOUNT_DATA_KEY,
  crate::notify_rules::MENTION_PREFERENCES_RECORD,
];
/// The key prefix of a conversation's bookmarks in [`Table::Account`].
const BOOKMARKS_PREFIX: &str = "bookmarks/";

/// A [`LocalStore`] that notes what its writes changed. Clones share the notes.
#[derive(Clone)]
pub struct TrackedStore<S> {
  inner: S,
  changes: Arc<Mutex<StoreChanges>>,
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

impl<S: LocalStore> TrackedStore<S> {
  pub fn new(inner: S) -> Self {
    Self {
      inner,
      changes: Arc::default(),
    }
  }

  /// The wrapped store (for what only it offers, like flushing to disk).
  pub fn inner(&self) -> &S {
    &self.inner
  }

  /// Everything noted since the last call; starts noting afresh.
  pub fn take_changes(&self) -> StoreChanges {
    std::mem::take(&mut *self.lock())
  }

  fn note(&self, change: Change) {
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
    if change != Change::Nothing && self.inner.get(table, key)?.as_deref() == Some(value) {
      return Ok(());
    }
    self.inner.put(table, key, value)?;
    self.note(change);
    Ok(())
  }

  fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
    self.inner.delete(table, key)?;
    self.note(classify(table, key));
    Ok(())
  }

  fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    self.inner.list(table)
  }

  fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
    self.inner.upsert_message(message)?;
    self.note(Change::Conversation(message.conversation_id));
    Ok(())
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

  fn messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.inner.messages(conversation, filter)
  }

  fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
    self.inner.delete_messages(conversation)?;
    self.note(Change::Conversation(conversation));
    Ok(())
  }

  fn purge_freed(&self) -> Result<(), StoreError> {
    self.inner.purge_freed()
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    self.inner.begin_writes()
  }

  fn end_writes(&self) -> Result<(), StoreError> {
    self.inner.end_writes()
  }

  fn write_soon(&self) {
    self.inner.write_soon();
  }
}

/// What a write to `table` under `key` may change in the snapshot.
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
    Table::Account => key
      .strip_prefix(BOOKMARKS_PREFIX)
      .map_or(Change::Nothing, conversation_of),
    // The presence log's head, a person's state, or their last seen (read by queries only).
    Table::Presence if key == crate::read_state::ANNOUNCED_PRESENCE_KEY => Change::Live,
    Table::Presence => key.parse().map_or(Change::Nothing, Change::User),
    Table::AnnouncedPresence => key.parse().map_or(Change::Live, Change::Announced),
    Table::Unread | Table::Threads | Table::ReadReceipts => Change::Live,
    Table::PinnedIdentities => Change::Security,
    // The "catching up" marks; the rest are sync cursors.
    Table::SyncState => key
      .strip_prefix(crate::client::CATCHING_UP_PREFIX)
      .map_or(Change::Nothing, conversation_of),
    Table::ContentKeys
    | Table::MlsStorage
    | Table::AppliedChanges
    | Table::Undecrypted
    | Table::PendingChanges
    | Table::SearchSegments
    | Table::SavedSearches
    | Table::FileTransfers
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

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
