//! The bookkeeping of the IndexedDB store's write batches (`browser/indexed_db.rs`), apart from the
//! browser so it can be tested natively: which writes are open, which wait for the next batch, how
//! many bytes they hold, and which batch carried which write.
//!
//! Of several writes of one record that wait for the same batch only the last one is issued, and a
//! replaced write is neither counted nor sealed (issue #326). A record whose seal fails is dropped
//! from every later batch and reported once by the next flush, so one bad record can't stop the
//! store from persisting anything else.
// Natively only the tests use it; the browser store uses all of it.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use std::collections::HashMap;

use super::StoreError;
use super::sealed::LazySeal;

/// Queued this much, the writes are issued right away instead of waiting for the batch delay.
pub(crate) const MAX_QUEUED_BYTES: usize = 8 * 1024 * 1024;
/// Queued this much when the batch delay ends, the batch waits a little longer to take the next
/// rewrite of its large record along (the MLS state, rewritten for every group a catch-up joins).
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
  maybe_last_failure: Option<WriteFailure>,
  /// Records dropped because their seal failed, until a flush reported them.
  unsealable: usize,
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

/// What one queued write puts into the batch being issued.
pub(crate) enum Issue<'a> {
  Put { sealed: &'a [u8], sequence: u32 },
  Delete { sequence: u32 },
}

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
  /// if it isn't queued any more. A record whose seal fails is dropped (logged, counted for the
  /// next flush), and the batch goes on without it.
  pub(crate) fn issue(&mut self, key: &str) -> Option<Issue<'_>> {
    let write = self
      .open
      .get_mut(key)
      .filter(|write| write.state == WriteState::Queued)?;
    if let Some(RecordValue::Unsealed(seal)) = &write.maybe_value {
      match seal() {
        Ok(sealed) => write.maybe_value = Some(RecordValue::Sealed(sealed)),
        Err(error) => {
          tracing::error!(%error, "could not seal a record for IndexedDB; it is not saved");
          self.open.remove(key);
          self.unsealable = self.unsealable.saturating_add(1);
          return None;
        },
      }
    }
    let write = self.open.get(key)?;
    match &write.maybe_value {
      Some(RecordValue::Sealed(sealed)) => Some(Issue::Put {
        sealed,
        sequence: write.sequence,
      }),
      // Sealed just above.
      Some(RecordValue::Unsealed(_)) => None,
      None => Some(Issue::Delete {
        sequence: write.sequence,
      }),
    }
  }

  /// The write of `key` went into a transaction.
  pub(crate) fn issued(&mut self, key: &str) {
    if let Some(write) = self.open.get_mut(key) {
      write.state = WriteState::Issued;
    }
  }

  /// Numbers the batch that carries `carried` (key and sequence of each write).
  pub(crate) fn start_batch(&mut self, carried: Vec<(String, u32)>) -> u32 {
    self.next_batch = self.next_batch.wrapping_add(1);
    self.batches.insert(self.next_batch, carried);
    self.next_batch
  }

  /// Only the latest write of a record closes it; an older one committing changes nothing.
  pub(crate) fn committed(&mut self, batch: u32) {
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

  /// The reason writes are failing while any failed write is open, or records were dropped for a
  /// failed seal since the last call (reported once).
  pub(crate) fn failure(&mut self) -> Result<(), StoreError> {
    let unsealable = std::mem::take(&mut self.unsealable);
    let failed = self
      .open
      .values()
      .filter(|write| write.state == WriteState::Failed)
      .count();
    if failed == 0 && !self.is_closed {
      if unsealable > 0 {
        return Err(StoreError::Database(format!(
          "{unsealable} records could not be sealed and were not saved"
        )));
      }
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
    assert!(matches!(writes.issue("mls"), Some(Issue::Put { sealed, .. }) if sealed == b"third"));
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

  #[test]
  fn a_record_that_does_not_seal_is_dropped_and_reported_once() {
    let mut writes = Writes::default();
    writes.queue(
      "bad".into(),
      Some(RecordValue::Unsealed(Box::new(failing_seal))),
      10,
    );
    writes.queue("good".into(), Some(sealed(b"good")), 10);
    let keys = writes.take_queued();
    assert!(writes.issue(&keys[0]).is_none());
    let Some(Issue::Put { sequence, .. }) = writes.issue(&keys[1]) else {
      panic!("the good record is issued");
    };
    writes.issued("good");
    let batch = writes.start_batch(vec![("good".into(), sequence)]);
    writes.committed(batch);
    assert!(matches!(writes.failure(), Err(StoreError::Database(_))));
    // Not carried into later batches, not reported again.
    writes.retry_failed();
    assert!(!writes.has_queued());
    assert!(writes.failure().is_ok());
  }

  #[test]
  fn a_failed_write_is_issued_again_with_its_sealed_value() {
    let mut writes = Writes::default();
    let seals = Rc::new(Cell::new(0));
    writes.queue("mls".into(), Some(lazy(&seals, b"state")), 10);
    writes.take_queued();
    let Some(Issue::Put { sequence, .. }) = writes.issue("mls") else {
      panic!("issued");
    };
    writes.issued("mls");
    let batch = writes.start_batch(vec![("mls".into(), sequence)]);
    writes.aborted(batch, WriteFailure::QuotaExceeded);
    assert!(matches!(writes.failure(), Err(StoreError::QuotaExceeded)));
    writes.retry_failed();
    assert_eq!(writes.take_queued(), vec!["mls".to_string()]);
    assert!(matches!(writes.issue("mls"), Some(Issue::Put { sealed, .. }) if sealed == b"state"));
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

  fn failing_seal() -> Result<Vec<u8>, StoreError> {
    Err(StoreError::Corrupt("unsealable".into()))
  }
}
