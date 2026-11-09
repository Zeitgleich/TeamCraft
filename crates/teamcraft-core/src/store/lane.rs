//! The store as one thread of the app sees it (issue #1476). The engine reads and writes through the
//! writing connection. A read thread reads through a connection of its own, which doesn't wait for
//! the engine's groups of writes, and writes through the writer's: the writer's caches stay valid,
//! and once the write committed the read connection takes a new snapshot, so the thread reads what
//! it wrote. Inside the thread's own group of writes its reads go to the writer, which holds them.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use teamcraft_proto::{ClientMessageId, ConversationId, MessageId, ThreadId};

use super::{
  Attached, AttachedRange, FeedVersion, LocalStore, MessageEdge, MessageFilter, MessageOrder, MessageSlot,
  SealedSqliteStore, StoreError, StoredMessage, Table, WriteTally, Written,
};
use crate::push_preview::ShownMessage;

/// A thread's view of a sealed SQLite store: the writer alone, or a reader beside it.
#[derive(Clone)]
pub struct LaneStore {
  writer: Arc<SealedSqliteStore>,
  maybe_reader: Option<Arc<SealedSqliteStore>>,
  /// Depth of this lane's groups of writes; inside one, reads go to the writer.
  writing: Arc<AtomicU32>,
}

impl LaneStore {
  /// The writing connection alone: the engine, and every thread of a process that opens no reader.
  pub fn writer(writer: Arc<SealedSqliteStore>) -> Self {
    Self {
      writer,
      maybe_reader: None,
      writing: Arc::default(),
    }
  }

  /// Reads through `reader` ([`SealedSqliteStore::open_reader`]) and writes through `writer`.
  pub fn reader(writer: Arc<SealedSqliteStore>, reader: Arc<SealedSqliteStore>) -> Self {
    Self {
      writer,
      maybe_reader: Some(reader),
      writing: Arc::default(),
    }
  }

  /// The connection reads go to now.
  fn reading(&self) -> &SealedSqliteStore {
    match &self.maybe_reader {
      Some(reader) if self.writing.load(Ordering::Acquire) == 0 => reader,
      _ => &self.writer,
    }
  }

  /// After a write outside a group: the reader sees it from here on.
  fn wrote(&self) {
    if self.writing.load(Ordering::Acquire) == 0
      && let Some(reader) = &self.maybe_reader
    {
      reader.renew_reads();
    }
  }

  fn write<T>(
    &self,
    write: impl FnOnce(&SealedSqliteStore) -> Result<T, StoreError>,
  ) -> Result<T, StoreError> {
    let result = write(&self.writer);
    self.wrote();
    result
  }
}

impl LocalStore for LaneStore {
  fn get(&self, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    self.reading().get(table, key)
  }

  fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError> {
    self.write(|writer| writer.put(table, key, value))
  }

  fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
    self.write(|writer| writer.delete(table, key))
  }

  fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    self.reading().list(table)
  }

  fn list_prefix(&self, table: Table, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    self.reading().list_prefix(table, prefix)
  }

  fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
    self.write(|writer| writer.upsert_message(message))
  }

  fn message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    self.reading().message(conversation, client_message_id)
  }

  fn messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.reading().messages(conversation, filter)
  }

  fn message_by_server_id(
    &self,
    conversation: ConversationId,
    server_id: MessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    self.reading().message_by_server_id(conversation, server_id)
  }

  fn shown_message_by_server_id(
    &self,
    conversation: ConversationId,
    server_id: MessageId,
  ) -> Result<Option<ShownMessage>, StoreError> {
    self.reading().shown_message_by_server_id(conversation, server_id)
  }

  fn visit_messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
    visit: &mut dyn FnMut(&StoredMessage),
  ) -> Result<(), StoreError> {
    self.reading().visit_messages(conversation, filter, visit)
  }

  fn with_all_messages(
    &self,
    conversation: ConversationId,
    read: &mut dyn FnMut(&[StoredMessage]),
  ) -> Result<(), StoreError> {
    self.reading().with_all_messages(conversation, read)
  }

  fn messages_at(
    &self,
    conversation: ConversationId,
    slot: MessageSlot,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.reading().messages_at(conversation, slot)
  }

  fn message_revision(&self, conversation: ConversationId) -> Option<u64> {
    self.reading().message_revision(conversation)
  }

  fn feed_version(&self, conversation: ConversationId) -> Result<Option<FeedVersion>, StoreError> {
    self.reading().feed_version(conversation)
  }

  fn written_cursor(&self) -> Option<u64> {
    self.reading().written_cursor()
  }

  fn is_awaited(&self) -> bool {
    self.writer.is_awaited()
  }

  fn snapshot_commit(&self) -> Option<u64> {
    self.maybe_reader.as_ref()?.snapshot_commit()
  }

  fn last_commit(&self) -> Option<u64> {
    self.writer.last_commit()
  }

  fn renew_snapshot(&self) {
    if let Some(reader) = &self.maybe_reader {
      reader.renew_reads();
    }
  }

  /// A reader's group of reads stays on the reader also while the thread writes: its reads go to
  /// the writer meanwhile, and the snapshot is taken again after the writes.
  fn begin_reads(&self) -> Result<(), StoreError> {
    match &self.maybe_reader {
      Some(reader) => reader.begin_reads(),
      None => self.writer.begin_reads(),
    }
  }

  fn end_reads(&self) {
    match &self.maybe_reader {
      Some(reader) => reader.end_reads(),
      None => self.writer.end_reads(),
    }
  }

  fn written_since(&self, conversation: ConversationId, cursor: u64) -> Option<Vec<Written>> {
    self.reading().written_since(conversation, cursor)
  }

  fn edge_server_id(
    &self,
    conversation: ConversationId,
    edge: MessageEdge,
  ) -> Result<Option<MessageId>, StoreError> {
    self.reading().edge_server_id(conversation, edge)
  }

  fn attached_messages(
    &self,
    conversation: ConversationId,
    kind: Attached,
    range: AttachedRange,
    limit: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.reading().attached_messages(conversation, kind, range, limit)
  }

  fn attachments_cursor(&self) -> Option<u64> {
    self.reading().attachments_cursor()
  }

  fn indexes_feed(&self) -> bool {
    self.reading().indexes_feed()
  }

  fn feed_len(&self, conversation: ConversationId) -> Result<usize, StoreError> {
    self.reading().feed_len(conversation)
  }

  fn feed_rows(
    &self,
    conversation: ConversationId,
    start: usize,
    len: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.reading().feed_rows(conversation, start, len)
  }

  fn feed_ranks(
    &self,
    conversation: ConversationId,
    orders: &[MessageOrder],
  ) -> Result<Vec<usize>, StoreError> {
    self.reading().feed_ranks(conversation, orders)
  }

  fn event_messages(&self, conversation: ConversationId) -> Result<Vec<StoredMessage>, StoreError> {
    self.reading().event_messages(conversation)
  }

  fn reactions_to(
    &self,
    conversation: ConversationId,
    targets: &[ClientMessageId],
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.reading().reactions_to(conversation, targets)
  }

  fn reply_counts(
    &self,
    conversation: ConversationId,
    threads: &[ThreadId],
  ) -> Result<Vec<usize>, StoreError> {
    self.reading().reply_counts(conversation, threads)
  }

  fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
    self.write(|writer| writer.delete_messages(conversation))
  }

  fn delete_message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<(), StoreError> {
    self.write(|writer| writer.delete_message(conversation, client_message_id))
  }

  fn purge_freed(&self) -> Result<(), StoreError> {
    self.writer.purge_freed()
  }

  fn purge_freed_soon(&self) {
    self.writer.purge_freed_soon();
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    self.writer.begin_writes()?;
    self.writing.fetch_add(1, Ordering::AcqRel);
    Ok(())
  }

  fn end_writes(&self) -> Result<(), StoreError> {
    let ended = self.writer.end_writes();
    // One thread per lane: nothing else moves the depth between the two steps.
    let was = self.writing.load(Ordering::Acquire);
    if was > 0 {
      self.writing.fetch_sub(1, Ordering::AcqRel);
    }
    if was <= 1 {
      self.wrote();
    }
    ended
  }

  fn fail_writes(&self) {
    self.writer.fail_writes();
  }

  fn write_soon(&self) {
    self.writer.write_soon();
  }

  /// The writer's: a commit of this app's own reader or writer is no other process's.
  fn write_tally(&self) -> Option<WriteTally> {
    self.writer.write_tally()
  }

  fn lost_deletes(&self, table: Table) -> Vec<String> {
    self.writer.lost_deletes(table)
  }
}

#[cfg(test)]
mod tests {
  use std::path::{Path, PathBuf};
  use std::sync::mpsc;

  use rand_core::{OsRng, RngCore};
  use teamcraft_crypto::secret::SymmetricKey;

  use super::*;
  use crate::store::tests::{id, message};
  use crate::store::{CommitJournal, SealedStoreLimits, StaticKeySource, TrackedStore, read_together};

  mod caches {
    use super::*;

    #[test]
    fn a_commit_of_the_writer_drops_only_what_it_changed_from_the_reader() {
      let lanes = Lanes::open();
      let (changed, untouched) = (id(10), id(11));
      lanes.writer.put(Table::Users, "a", b"1").unwrap();
      lanes.writer.put(Table::Users, "b", b"1").unwrap();
      lanes
        .writer
        .upsert_message(&message(changed, 1, Some(10), None, 1))
        .unwrap();
      lanes
        .writer
        .upsert_message(&message(untouched, 1, Some(20), None, 1))
        .unwrap();
      assert_eq!(lanes.reader.get(Table::Users, "a").unwrap(), Some(b"1".to_vec()));
      assert_eq!(
        lanes.reader.messages(changed, MessageFilter::All).unwrap().len(),
        1
      );
      let (changed_before, untouched_before) = (
        lanes.reader.message_revision(changed),
        lanes.reader.message_revision(untouched),
      );

      lanes.writer.put(Table::Users, "a", b"2").unwrap();
      lanes
        .writer
        .upsert_message(&message(changed, 2, Some(11), None, 2))
        .unwrap();

      assert_eq!(lanes.reader.get(Table::Users, "a").unwrap(), Some(b"2".to_vec()));
      assert_eq!(lanes.reader.get(Table::Users, "b").unwrap(), Some(b"1".to_vec()));
      assert_eq!(
        lanes.reader.messages(changed, MessageFilter::All).unwrap().len(),
        2
      );
      assert_eq!(lanes.reader.feed_len(changed).unwrap(), 2);
      assert_ne!(lanes.reader.message_revision(changed), changed_before);
      // Not dropped: the caches stayed warm across the commit (a drop raises every revision).
      assert_eq!(lanes.reader.message_revision(untouched), untouched_before);
      lanes.finish();
    }

    #[test]
    fn a_commit_of_another_process_drops_everything_and_is_seen() {
      let lanes = Lanes::open();
      let conversation = id(10);
      lanes.writer.put(Table::Users, "a", b"1").unwrap();
      assert_eq!(lanes.reader.get(Table::Users, "a").unwrap(), Some(b"1".to_vec()));
      let before = lanes.reader.message_revision(conversation);
      // The notification extension: a connection of its own, which stages nothing.
      let other = SealedSqliteStore::open(&lanes.path, &lanes.key).unwrap();
      other.put(Table::Users, "a", b"other").unwrap();
      drop(other);
      assert_eq!(
        lanes.reader.get(Table::Users, "a").unwrap(),
        Some(b"other".to_vec())
      );
      assert_ne!(lanes.reader.message_revision(conversation), before);
      // The writer's next commit is replayed again.
      lanes.writer.put(Table::Users, "a", b"3").unwrap();
      assert_eq!(lanes.reader.get(Table::Users, "a").unwrap(), Some(b"3".to_vec()));
      lanes.finish();
    }

    #[test]
    fn a_rolled_back_group_is_never_seen_and_its_number_is_taken_again() {
      let lanes = Lanes::open();
      lanes.writer.put(Table::Users, "a", b"old").unwrap();
      assert_eq!(
        lanes.reader.get(Table::Users, "a").unwrap(),
        Some(b"old".to_vec())
      );
      lanes.writer.begin_writes().unwrap();
      lanes.writer.put(Table::Users, "a", b"lost").unwrap();
      lanes.writer.fail_writes();
      lanes.writer.end_writes().unwrap_err();
      assert_eq!(
        lanes.reader.get(Table::Users, "a").unwrap(),
        Some(b"old".to_vec())
      );
      lanes.writer.put(Table::Users, "a", b"new").unwrap();
      assert_eq!(
        lanes.reader.get(Table::Users, "a").unwrap(),
        Some(b"new".to_vec())
      );
      lanes.finish();
    }
  }

  mod lanes {
    use super::*;

    /// Issue #1583: a long read on the reader holds up neither the writer nor its commit.
    #[test]
    fn a_long_read_on_the_reader_does_not_hold_up_the_writer() {
      let lanes = Lanes::open();
      lanes.writer.put(Table::Users, "a", b"old").unwrap();
      let lane = LaneStore::reader(Arc::clone(&lanes.writer), Arc::clone(&lanes.reader));
      lane.begin_reads().unwrap();
      assert_eq!(lane.get(Table::Users, "a").unwrap(), Some(b"old".to_vec()));
      let writer = Arc::clone(&lanes.writer);
      let (done, wrote) = mpsc::channel();
      let engine = std::thread::spawn(move || {
        writer.put(Table::Users, "a", b"new").unwrap();
        done.send(()).unwrap();
      });
      // On one connection this would wait for the read group to end.
      wrote.recv().unwrap();
      engine.join().unwrap();
      assert_eq!(
        lane.get(Table::Users, "a").unwrap(),
        Some(b"old".to_vec()),
        "the read group keeps its snapshot"
      );
      lane.end_reads();
      assert_eq!(lane.get(Table::Users, "a").unwrap(), Some(b"new".to_vec()));
      drop(lane);
      lanes.finish();
    }

    #[test]
    fn a_read_thread_reads_what_it_wrote_inside_its_read_group() {
      let lanes = Lanes::open();
      let lane = LaneStore::reader(Arc::clone(&lanes.writer), Arc::clone(&lanes.reader));
      let conversation = id(10);
      read_together(&lane, || {
        assert_eq!(lane.get(Table::Users, "a")?, None);
        lane.put(Table::Users, "a", b"mine")?;
        assert_eq!(lane.get(Table::Users, "a")?, Some(b"mine".to_vec()));
        lane.begin_writes()?;
        lane.upsert_message(&message(conversation, 1, Some(10), None, 1))?;
        assert_eq!(
          lane.messages(conversation, MessageFilter::All)?.len(),
          1,
          "inside the group"
        );
        lane.end_writes()?;
        assert_eq!(
          lane.messages(conversation, MessageFilter::All)?.len(),
          1,
          "after it"
        );
        Ok::<_, StoreError>(())
      })
      .unwrap();
      drop(lane);
      lanes.finish();
    }
  }

  mod notes {
    use super::*;

    /// A reader on its own connection never takes a note of a write its snapshot doesn't hold:
    /// not of an open group, and not of one committed after its snapshot began.
    #[test]
    fn notes_of_a_group_reach_a_reader_after_its_commit_with_a_snapshot_that_holds_it() {
      let lanes = Lanes::open();
      let engine = TrackedStore::with_message_cache(LaneStore::writer(Arc::clone(&lanes.writer)), 1 << 20);
      let reader = engine.sharing(LaneStore::reader(
        Arc::clone(&lanes.writer),
        Arc::clone(&lanes.reader),
      ));
      let conversation = id(10);
      engine
        .upsert_message(&message(conversation, 1, Some(10), None, 1))
        .unwrap();
      reader.take_changes();
      reader.begin_reads().unwrap();
      let cursor = reader.written_cursor().unwrap();
      assert_eq!(
        reader.messages(conversation, MessageFilter::All).unwrap().len(),
        1
      );

      let (in_group, opened) = mpsc::channel();
      let (end_group, ended) = mpsc::channel::<()>();
      let writer = {
        let engine = engine.clone();
        std::thread::spawn(move || {
          engine.begin_writes().unwrap();
          engine
            .upsert_message(&message(conversation, 2, Some(11), None, 2))
            .unwrap();
          in_group.send(()).unwrap();
          ended.recv().unwrap();
          engine.end_writes().unwrap();
        })
      };
      opened.recv().unwrap();
      assert_eq!(
        reader.take_changes(),
        StoreChanges::default(),
        "the group is open"
      );
      assert_eq!(reader.written_since(conversation, cursor), Some(Vec::new()));
      assert_eq!(
        reader.message_revision(conversation),
        None,
        "nothing kept meanwhile"
      );
      end_group.send(()).unwrap();
      writer.join().unwrap();

      // The snapshot began before the commit: taking its notes takes a new one.
      assert_eq!(reader.take_changes().conversations, [conversation].into());
      assert_eq!(
        reader
          .written_since(conversation, cursor)
          .map(|written| written.len()),
        Some(1)
      );
      assert_eq!(
        reader.messages(conversation, MessageFilter::All).unwrap().len(),
        2
      );
      assert!(reader.message_revision(conversation).is_some());
      reader.end_reads();
      drop((engine, reader));
      lanes.finish();
    }
  }

  /// A writing connection and a reading one on one database file.
  struct Lanes {
    writer: Arc<SealedSqliteStore>,
    reader: Arc<SealedSqliteStore>,
    key: StaticKeySource,
    path: PathBuf,
    directory: PathBuf,
  }

  impl Lanes {
    fn open() -> Self {
      let directory = std::env::temp_dir().join(format!("teamcraft-lanes-{}", OsRng.next_u64()));
      std::fs::create_dir_all(&directory).unwrap();
      let path = directory.join("store.db");
      let key = StaticKeySource::new(SymmetricKey::generate(&mut OsRng));
      let journal = CommitJournal::new();
      let writer = SealedSqliteStore::open(&path, &key).unwrap();
      writer.publish_commits(Arc::clone(&journal));
      let reader = open_reader(&path, &key, journal);
      Self {
        writer: Arc::new(writer),
        reader: Arc::new(reader),
        key,
        path,
        directory,
      }
    }

    fn finish(self) {
      drop((self.writer, self.reader));
      std::fs::remove_dir_all(&self.directory).unwrap();
    }
  }

  fn open_reader(path: &Path, key: &StaticKeySource, journal: Arc<CommitJournal>) -> SealedSqliteStore {
    SealedSqliteStore::open_reader(path, key, SealedStoreLimits::default(), journal).unwrap()
  }

  use crate::store::StoreChanges;
}
