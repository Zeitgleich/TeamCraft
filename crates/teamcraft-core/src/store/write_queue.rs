//! The bookkeeping of the IndexedDB store's write batches (`browser/indexed_db.rs`), apart from the
//! browser so it can be tested natively: which writes are open, which wait for the next batch, how
//! many bytes they hold, and which batch carried which write.
//!
//! Of several writes of one record that wait for the same batch only the last one is issued, and a
//! replaced write is neither counted nor sealed (issue #326).
//!
//! One batch runs at a time ([`Writes::may_issue`]): the next is issued when it ended, and takes what
//! it failed to write along. IndexedDB keeps transactions in order, but one that aborts (quota) doesn't
//! stop the next: with two running, the second would commit without the first, and what survives the
//! page would no longer be a prefix of the writes (issue #840). A record whose seal fails fails its
//! batch the same way: nothing of the batch is written, and it is issued again until it goes through.
// Natively only the tests use it; the browser store uses all of it.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use std::collections::HashMap;

use super::StoreError;
use super::sealed::LazySeal;

/// Queued this much, the writes are issued right away instead of waiting for the batch delay.
pub(crate) const MAX_QUEUED_BYTES: usize = 8 * 1024 * 1024;
/// Queued this much when the batch delay ends, the batch waits a little longer to take the next
/// rewrite of its large record along (#326: the MLS state was one such record until #424).
pub(crate) const LARGE_BATCH_BYTES: usize = 1024 * 1024;

/// Writes that have not committed yet, by record key.
#[derive(Default)]
pub(crate) struct Writes {
  pub(crate) next_sequence: u32,
  open: HashMap<String, OpenWrite>,
  /// Keys whose latest write waits for the next batch, each once.
  queued: Vec<String>,
  queued_bytes: usize,
  /// A batch timer is running.
  pub(crate) is_scheduled: bool,
  next_batch: u32,
  /// The writes each issued transaction carries, by batch number.
  batches: HashMap<u32, Vec<(String, u32)>>,
  /// The batch whose transaction has not ended yet.
  maybe_running_batch: Option<u32>,
  /// The batch that ended last; batches end in the order of their numbers, one at a time.
  last_ended_batch: u32,
  maybe_last_failure: Option<WriteFailure>,
  /// Another page (a newer version) asked for the database; it was closed, writes wait for a reload.
  pub(crate) is_closed: bool,
  /// Depth of the open group of writes ([`crate::store::LocalStore::begin_writes`]): the size limit
  /// doesn't issue a batch in the middle of one.
  pub(crate) group_depth: u32,
}

/// A record's bytes, sealed when its batch is issued.
pub(crate) enum RecordValue {
  Sealed(Vec<u8>),
  Unsealed(LazySeal),
}

/// What an open write of a record does ([`Writes::latest`]).
pub(crate) enum OpenValue {
  /// Puts these sealed bytes.
  Put(Vec<u8>),
  Delete,
}

/// What one queued write puts into the batch being issued.
pub(crate) enum Issue<'a> {
  Put { sealed: &'a [u8], sequence: u32 },
  Delete { sequence: u32 },
}

/// A record's seal failed; the batch must not go out without it.
#[derive(Debug)]
pub(crate) struct SealFailed;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WriteFailure {
  QuotaExceeded,
  Closed,
  Other,
}

/// The latest write of one record. Its value is kept until it committed, to write it again.
struct OpenWrite {
  sequence: u32,
  /// `None` deletes the record.
  maybe_value: Option<RecordValue>,
  /// What the write counts towards [`MAX_QUEUED_BYTES`] while it is queued.
  bytes: usize,
  state: WriteState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriteState {
  /// Waits for the next batch.
  Queued,
  /// In a transaction that has not finished.
  Issued,
  /// Refused or aborted; issued again by the next flush.
  Failed,
}

impl Writes {
  /// Records the write as the record's latest and queues its key, once. A queued write it replaces
  /// no longer counts towards [`MAX_QUEUED_BYTES`] and is never sealed. Returns whether the queue
  /// should be issued now.
  pub(crate) fn queue(&mut self, key: String, maybe_value: Option<RecordValue>, bytes: usize) -> bool {
    self.next_sequence = self.next_sequence.wrapping_add(1);
    self.queued_bytes = self.queued_bytes.saturating_add(bytes);
    let write = OpenWrite {
      sequence: self.next_sequence,
      maybe_value,
      bytes,
      state: WriteState::Queued,
    };
    let maybe_replaced = self
      .open
      .insert(key.clone(), write)
      .filter(|earlier| earlier.state == WriteState::Queued);
    match maybe_replaced {
      Some(replaced) => self.queued_bytes = self.queued_bytes.saturating_sub(replaced.bytes),
      None => self.queued.push(key),
    }
    self.group_depth == 0 && self.is_over_limit()
  }

  /// Whether a batch may be issued now: none is running, and no group of writes is open. Groups run
  /// synchronously, so the only group still open here is one a panic ended (in the browser a panic
  /// doesn't unwind): what it wrote so far must never reach the disk.
  pub(crate) fn may_issue(&self) -> bool {
    self.maybe_running_batch.is_none() && self.group_depth == 0
  }

  /// The latest write of `key` that hasn't committed, if one is open; a put waiting for its batch
  /// is sealed now (the batch then takes the sealed bytes).
  pub(crate) fn latest(&mut self, key: &str) -> Result<Option<OpenValue>, StoreError> {
    let Some(write) = self.open.get_mut(key) else {
      return Ok(None);
    };
    if let Some(RecordValue::Unsealed(seal)) = &write.maybe_value {
      write.maybe_value = Some(RecordValue::Sealed(seal()?));
    }
    Ok(Some(match &write.maybe_value {
      Some(RecordValue::Sealed(sealed)) => OpenValue::Put(sealed.clone()),
      // Sealed just above.
      Some(RecordValue::Unsealed(_)) | None => OpenValue::Delete,
    }))
  }

  /// Whether so much is queued that it should be issued now ([`MAX_QUEUED_BYTES`]).
  pub(crate) fn is_over_limit(&self) -> bool {
    self.queued_bytes >= MAX_QUEUED_BYTES
  }

  /// The database was closed for a newer version: writes wait for a reload.
  pub(crate) fn close(&mut self) {
    self.is_closed = true;
    self.maybe_last_failure = Some(WriteFailure::Closed);
  }

  /// Whether a batch whose delay ended should wait a little longer ([`LARGE_BATCH_BYTES`]).
  pub(crate) fn is_large(&self) -> bool {
    self.queued_bytes >= LARGE_BATCH_BYTES
  }

  pub(crate) fn has_queued(&self) -> bool {
    !self.queued.is_empty()
  }

  /// The keys of the next batch; the queue starts over.
  pub(crate) fn take_queued(&mut self) -> Vec<String> {
    self.is_scheduled = false;
    self.queued_bytes = 0;
    std::mem::take(&mut self.queued)
  }

  /// Queues every failed write again, with its latest value.
  pub(crate) fn retry_failed(&mut self) {
    let failed: Vec<String> = self
      .open
      .iter()
      .filter(|(_, write)| write.state == WriteState::Failed)
      .map(|(key, _)| key.clone())
      .collect();
    for key in failed {
      if let Some(write) = self.open.get_mut(&key) {
        write.state = WriteState::Queued;
      }
      self.queued.push(key);
    }
  }

  /// What the queued write of `key` puts into the batch, sealed now if it waited for it; `None`
  /// if it isn't queued any more. A record whose seal fails stops the batch: the caller fails it, the
  /// write stays open with its seal, and the cache and the disk still end up the same.
  pub(crate) fn issue(&mut self, key: &str) -> Result<Option<Issue<'_>>, SealFailed> {
    let Some(write) = self
      .open
      .get_mut(key)
      .filter(|write| write.state == WriteState::Queued)
    else {
      return Ok(None);
    };
    if let Some(RecordValue::Unsealed(seal)) = &write.maybe_value {
      match seal() {
        Ok(sealed) => write.maybe_value = Some(RecordValue::Sealed(sealed)),
        Err(error) => {
          tracing::error!(%error, "could not seal a record for IndexedDB; its batch waits");
          return Err(SealFailed);
        },
      }
    }
    Ok(match &write.maybe_value {
      Some(RecordValue::Sealed(sealed)) => Some(Issue::Put {
        sealed,
        sequence: write.sequence,
      }),
      // Sealed just above.
      Some(RecordValue::Unsealed(_)) => None,
      None => Some(Issue::Delete {
        sequence: write.sequence,
      }),
    })
  }

  /// The write of `key` went into a transaction.
  pub(crate) fn issued(&mut self, key: &str) {
    if let Some(write) = self.open.get_mut(key) {
      write.state = WriteState::Issued;
    }
  }

  /// Numbers the batch that carries `carried` (key and sequence of each write); it runs until
  /// [`Self::committed`] or [`Self::aborted`].
  pub(crate) fn start_batch(&mut self, carried: Vec<(String, u32)>) -> u32 {
    self.next_batch = self.next_batch.wrapping_add(1);
    self.batches.insert(self.next_batch, carried);
    self.maybe_running_batch = Some(self.next_batch);
    self.next_batch
  }

  /// What a flush has to wait for: the batch that holds everything written so far, once it ended
  /// ([`Self::is_past`]). `None`: no batch is running or due.
  pub(crate) fn last_batch_due(&self) -> Option<u32> {
    let running = self.maybe_running_batch?;
    Some(if self.has_queued() {
      running.wrapping_add(1)
    } else {
      running
    })
  }

  /// Whether nothing written before [`Self::last_batch_due`] said `batch` is still on its way: that
  /// batch ended, or nothing is running or queued (a batch that couldn't start fails its writes).
  pub(crate) fn is_past(&self, maybe_batch: Option<u32>) -> bool {
    let is_idle = self.maybe_running_batch.is_none() && !self.has_queued();
    // Batch numbers wrap, so "ended" is a distance, not a comparison.
    is_idle || maybe_batch.is_none_or(|batch| self.last_ended_batch.wrapping_sub(batch) < u32::MAX / 2)
  }

  /// Only the latest write of a record closes it; an older one committing changes nothing.
  pub(crate) fn committed(&mut self, batch: u32) {
    self.ended(batch);
    for (key, sequence) in self.batches.remove(&batch).unwrap_or_default() {
      if self
        .open
        .get(&key)
        .is_some_and(|write| write.sequence == sequence && write.state == WriteState::Issued)
      {
        self.open.remove(&key);
      }
    }
  }

  pub(crate) fn aborted(&mut self, batch: u32, failure: WriteFailure) {
    self.ended(batch);
    for (key, sequence) in self.batches.remove(&batch).unwrap_or_default() {
      if let Some(write) = self.open.get_mut(&key)
        && write.sequence == sequence
        && write.state == WriteState::Issued
      {
        write.state = WriteState::Failed;
        self.maybe_last_failure = Some(failure);
      }
    }
  }

  fn ended(&mut self, batch: u32) {
    if self.maybe_running_batch == Some(batch) {
      self.maybe_running_batch = None;
    }
    self.last_ended_batch = batch;
  }

  /// A write of the batch that was aborted before it was sent (queued or already issued).
  pub(crate) fn fail_open(&mut self, key: &str, failure: WriteFailure) {
    if let Some(write) = self.open.get_mut(key)
      && write.state != WriteState::Failed
    {
      write.state = WriteState::Failed;
      self.maybe_last_failure = Some(failure);
    }
  }

  pub(crate) fn fail_queued(&mut self, key: &str, failure: WriteFailure) {
    if let Some(write) = self.open.get_mut(key)
      && write.state == WriteState::Queued
    {
      write.state = WriteState::Failed;
      self.maybe_last_failure = Some(failure);
    }
  }

  /// The reason writes are failing while any failed write is open.
  pub(crate) fn failure(&self) -> Result<(), StoreError> {
    let failed = self
      .open
      .values()
      .filter(|write| write.state == WriteState::Failed)
      .count();
    if failed == 0 && !self.is_closed {
      return Ok(());
    }
    Err(match self.maybe_last_failure.unwrap_or(WriteFailure::Other) {
      WriteFailure::QuotaExceeded => StoreError::QuotaExceeded,
      WriteFailure::Closed => StoreError::Closed,
      WriteFailure::Other => StoreError::Database(format!("{failed} IndexedDB writes failed")),
    })
  }
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;
  use std::collections::BTreeSet;
  use std::rc::Rc;

  use super::*;

  #[test]
  fn a_replaced_write_is_neither_counted_nor_sealed() {
    let mut writes = Writes::default();
    let seals = Rc::new(Cell::new(0));
    assert!(!writes.queue("mls".into(), Some(lazy(&seals, b"first")), 3 * MIB));
    assert!(!writes.queue("mls".into(), Some(lazy(&seals, b"second")), 3 * MIB));
    assert!(!writes.queue("mls".into(), Some(lazy(&seals, b"third")), 3 * MIB));
    // Three copies of 3 MiB used to count 9 MiB and issue at once.
    assert!(writes.is_large());
    let keys = writes.take_queued();
    assert_eq!(keys, vec!["mls".to_string()]);
    assert!(matches!(writes.issue("mls"), Ok(Some(Issue::Put { sealed, .. })) if sealed == b"third"));
    assert_eq!(seals.get(), 1);
  }

  #[test]
  fn much_queued_is_issued_at_once() {
    let mut writes = Writes::default();
    assert!(!writes.queue("a".into(), Some(sealed(b"a")), MAX_QUEUED_BYTES / 2));
    assert!(writes.queue("b".into(), Some(sealed(b"b")), MAX_QUEUED_BYTES / 2));
    writes.take_queued();
    assert!(!writes.is_large());
  }

  #[test]
  fn a_group_of_writes_is_not_split_by_the_size_limit() {
    let mut writes = Writes {
      group_depth: 1,
      ..Writes::default()
    };
    assert!(!writes.queue("a".into(), Some(sealed(b"a")), MAX_QUEUED_BYTES));
    assert!(!writes.queue("b".into(), Some(sealed(b"b")), 1));
    // The group ended: its writes go out together.
    writes.group_depth = 0;
    assert!(writes.is_over_limit());
    assert_eq!(writes.take_queued().len(), 2);
  }

  #[test]
  fn only_a_batch_over_the_large_size_waits_longer() {
    let mut writes = Writes::default();
    writes.queue("small".into(), Some(sealed(b"s")), LARGE_BATCH_BYTES - 1);
    assert!(!writes.is_large());
    writes.queue("more".into(), Some(sealed(b"m")), 1);
    assert!(writes.is_large());
  }

  /// Issue #840: dropping the record kept it in the cache and committed the rest of its group, so
  /// the disk held half a group and memory something else, without anybody finding out in time.
  #[test]
  fn a_record_that_does_not_seal_holds_its_batch_back_and_is_issued_again() {
    let mut disk = Disk::default();
    let is_sealable = Rc::new(Cell::new(false));
    let mut writes = Writes::default();
    writes.queue("good before".into(), Some(sealed(b"1")), 1);
    writes.queue("bad".into(), Some(flaky_seal(&is_sealable, b"2")), 1);
    writes.queue("good after".into(), Some(sealed(b"3")), 1);

    issue_batch(&mut writes, &mut disk);
    assert!(disk.committed.is_empty(), "nothing of the batch");
    assert!(matches!(writes.failure(), Err(StoreError::Database(_))));

    // Still failing: reported again, still nothing on disk.
    writes.retry_failed();
    issue_batch(&mut writes, &mut disk);
    assert!(disk.committed.is_empty());
    assert!(writes.failure().is_err());

    is_sealable.set(true);
    writes.retry_failed();
    issue_batch(&mut writes, &mut disk);
    writes.committed(disk.last_batch());
    disk.commit_last();
    assert_eq!(disk.committed.len(), 3, "the whole batch");
    assert!(writes.failure().is_ok());
  }

  mod one_batch_at_a_time {
    use super::*;

    /// Issue #840: while a batch runs the next one waits, so one that aborts can't be overtaken.
    #[test]
    fn a_later_write_never_reaches_the_disk_without_a_batch_that_aborted() {
      let mut writes = Writes::default();
      let mut disk = Disk::default();
      writes.queue("first".into(), Some(sealed(b"1")), 1);
      issue_batch(&mut writes, &mut disk);
      let first = disk.last_batch();

      writes.queue("second".into(), Some(sealed(b"2")), 1);
      assert!(!writes.may_issue(), "the first batch runs");

      writes.aborted(first, WriteFailure::QuotaExceeded);
      disk.abort_last();
      assert!(writes.may_issue());
      // What the store does when a batch ended: the failed writes go with the next one.
      writes.retry_failed();
      issue_batch(&mut writes, &mut disk);
      writes.committed(disk.last_batch());
      disk.commit_last();

      assert_eq!(
        disk.committed,
        ["first", "second"].map(String::from).into_iter().collect()
      );
      assert!(writes.failure().is_ok());
    }

    #[test]
    fn a_batch_that_keeps_aborting_leaves_nothing_later_on_the_disk() {
      let mut writes = Writes::default();
      let mut disk = Disk::default();
      writes.queue("first".into(), Some(sealed(b"1")), 1);
      issue_batch(&mut writes, &mut disk);
      for attempt in 0..3 {
        writes.queue(format!("later {attempt}"), Some(sealed(b"2")), 1);
        writes.aborted(disk.last_batch(), WriteFailure::QuotaExceeded);
        disk.abort_last();
        assert!(matches!(writes.failure(), Err(StoreError::QuotaExceeded)));
        writes.retry_failed();
        issue_batch(&mut writes, &mut disk);
      }
      assert!(disk.committed.is_empty());
    }

    /// In the browser a panic doesn't unwind: a group it ended stays open, and the batch timer must
    /// not write its first half.
    #[test]
    fn nothing_is_issued_while_a_group_is_open() {
      let mut writes = Writes {
        group_depth: 1,
        ..Writes::default()
      };
      writes.queue("half of a group".into(), Some(sealed(b"1")), 1);
      assert!(!writes.may_issue());
    }

    /// A flush waits for the batch that holds what was queued when it began, not for what is
    /// written while it waits.
    #[test]
    fn a_flush_waits_for_the_batch_of_what_was_queued_not_for_later_ones() {
      let mut writes = Writes::default();
      let mut disk = Disk::default();
      assert!(writes.is_past(writes.last_batch_due()), "nothing to wait for");

      writes.queue("first".into(), Some(sealed(b"1")), 1);
      issue_batch(&mut writes, &mut disk);
      let first = disk.last_batch();
      writes.queue("second".into(), Some(sealed(b"2")), 1);
      let due = writes.last_batch_due();
      assert_eq!(due, Some(first + 1));
      assert!(!writes.is_past(due));

      writes.committed(first);
      disk.commit_last();
      assert!(!writes.is_past(due), "the second is still queued");
      issue_batch(&mut writes, &mut disk);
      assert!(!writes.is_past(due), "and runs");

      writes.queue("third".into(), Some(sealed(b"3")), 1);
      writes.committed(disk.last_batch());
      disk.commit_last();
      assert!(writes.is_past(due), "the third is not what the flush waited for");
    }

    /// A batch whose transaction can't even start leaves nothing running: the flush doesn't wait
    /// for it for ever.
    #[test]
    fn a_batch_that_could_not_start_is_not_waited_for() {
      let mut writes = Writes::default();
      let mut disk = Disk::default();
      writes.queue("first".into(), Some(sealed(b"1")), 1);
      issue_batch(&mut writes, &mut disk);
      writes.queue("second".into(), Some(sealed(b"2")), 1);
      let due = writes.last_batch_due();

      writes.committed(disk.last_batch());
      disk.commit_last();
      for key in writes.take_queued() {
        writes.fail_queued(&key, WriteFailure::Closed);
      }
      assert!(writes.is_past(due));
      assert!(writes.failure().is_err());
    }
  }

  #[test]
  fn a_failed_write_is_issued_again_with_its_sealed_value() {
    let mut writes = Writes::default();
    let seals = Rc::new(Cell::new(0));
    writes.queue("mls".into(), Some(lazy(&seals, b"state")), 10);
    writes.take_queued();
    let Ok(Some(Issue::Put { sequence, .. })) = writes.issue("mls") else {
      panic!("issued");
    };
    writes.issued("mls");
    let batch = writes.start_batch(vec![("mls".into(), sequence)]);
    writes.aborted(batch, WriteFailure::QuotaExceeded);
    assert!(matches!(writes.failure(), Err(StoreError::QuotaExceeded)));
    writes.retry_failed();
    assert_eq!(writes.take_queued(), vec!["mls".to_string()]);
    assert!(matches!(writes.issue("mls"), Ok(Some(Issue::Put { sealed, .. })) if sealed == b"state"));
    assert_eq!(seals.get(), 1, "sealed once, kept for the retry");
  }

  const MIB: usize = 1024 * 1024;

  fn lazy(seals: &Rc<Cell<u32>>, bytes: &'static [u8]) -> RecordValue {
    let seals = seals.clone();
    RecordValue::Unsealed(Box::new(move || {
      seals.set(seals.get() + 1);
      Ok(bytes.to_vec())
    }))
  }

  fn sealed(bytes: &[u8]) -> RecordValue {
    RecordValue::Sealed(bytes.to_vec())
  }

  fn flaky_seal(is_sealable: &Rc<Cell<bool>>, bytes: &'static [u8]) -> RecordValue {
    let is_sealable = is_sealable.clone();
    RecordValue::Unsealed(Box::new(move || {
      if is_sealable.get() {
        Ok(bytes.to_vec())
      } else {
        Err(StoreError::Corrupt("unsealable".into()))
      }
    }))
  }

  /// What IndexedDB holds, and the batches its transactions carry.
  #[derive(Default)]
  struct Disk {
    committed: BTreeSet<String>,
    batches: Vec<(u32, Vec<String>)>,
  }

  impl Disk {
    fn last_batch(&self) -> u32 {
      self.batches.last().unwrap().0
    }

    fn commit_last(&mut self) {
      self.committed.extend(self.batches.pop().unwrap().1);
    }

    fn abort_last(&mut self) {
      self.batches.pop();
    }
  }

  /// What the store does to issue a batch: every queued write, or none if a seal fails.
  fn issue_batch(writes: &mut Writes, disk: &mut Disk) {
    assert!(writes.may_issue());
    let keys = writes.take_queued();
    let mut carried = Vec::new();
    for key in &keys {
      let sequence = match writes.issue(key) {
        Ok(Some(Issue::Put { sequence, .. } | Issue::Delete { sequence })) => sequence,
        Ok(None) => continue,
        Err(SealFailed) => {
          for key in &keys {
            writes.fail_open(key, WriteFailure::Other);
          }
          return;
        },
      };
      writes.issued(key);
      carried.push((key.clone(), sequence));
    }
    let batch = writes.start_batch(carried);
    disk.batches.push((batch, keys));
  }
}
