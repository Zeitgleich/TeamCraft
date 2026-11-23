//! Sealed records in an append-only file with a checkpointed index (issue #2266, e2ee.md §9.1).
//!
//! The browser's store kept every record's ciphertext in memory because IndexedDB can't be read
//! synchronously and [`crate::store::LocalStore`] is synchronous: opening read the whole store.
//! A dedicated worker can read the origin private file system (OPFS) synchronously, so there the
//! records stay on disk and are read when the store asks for one. This module is the file format,
//! independent of the browser so it is tested natively; `browser::opfs` puts it on OPFS.
//!
//! **Files.** Two log slots and one index. A slot starts with a header naming its generation; the
//! valid slot with the highest generation is the log, the other is empty or left over. The log is
//! a sequence of groups, each a run of frames (a record put, a record deleted) closed by a commit
//! frame carrying the group's frame count and CRC-32. A group whose commit is missing or doesn't
//! match (the browser died while writing it) is cut off when the log is opened, so what survives is
//! always a prefix of the groups in their order, never part of one (as with IndexedDB, issue #840).
//!
//! **Index.** Where each record's bytes are, in memory: 32 bytes of ID and 12 of place per record,
//! not its ciphertext. The index file holds a copy (a checkpoint) and how much of the log it
//! covers; opening reads it and scans only the log after it. A checkpoint is written once that tail
//! grows past [`LogLimits::checkpoint_bytes`] (or the index's own size, so rewriting the index
//! costs at most as much as the log written meanwhile) and when the owner asks
//! ([`RecordLog::checkpoint`], the page being hidden). A damaged or outdated index file only costs a
//! scan of the whole log.
//!
//! **Compaction.** Rewrites and deletes leave old frames behind. Once the log is more than twice
//! what it holds (and at least [`LogLimits::compact_bytes`]), the live records are copied into the
//! other slot as one group, its header written last with the next generation, and the old slot is
//! emptied. A compaction interrupted before the new header is on disk leaves a slot without a valid
//! header, which the next open ignores.
//!
//! **Failed writes.** A write the disk refuses (the origin's quota) loses nothing: the group stays
//! in memory (`unsaved`), reads see it, and it is written again with the next group after a backoff
//! (1 s, doubled up to 30 s), as one group with everything after it, so the order holds. The owner
//! hears it from [`RecordLog::flush`] and pauses what it can fetch again later past
//! [`LogLimits::max_unsaved`] records ([`RecordLog::is_backlogged`]): the IndexedDB store's
//! semantics (e2ee.md §9.4), so the web core above doesn't change.
//!
//! The file holds only what the sealed store hands it: pseudorandom lookup IDs and sealed bytes.
//! The CRC finds torn writes, not tampering; a record moved or altered fails its seal, whose
//! associated data is its ID (e2ee.md §9.1).
// Natively only its tests use it; the browser's store is its user.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::Bound;
use std::time::Duration;

use teamcraft_crypto::local_store::{LOOKUP_ID_LEN, LookupId};

/// Opens a log slot: format and version.
const LOG_MAGIC: [u8; 8] = *b"TCLOG\0\0\x01";
/// Opens the index file.
const INDEX_MAGIC: [u8; 8] = *b"TCIDX\0\0\x01";
/// Magic, generation, CRC of both.
const HEADER_LEN: u64 = 8 + 8 + 4;
/// The bytes at the start of a slot that say whether it holds the log: a reader that copies the
/// files ([`CopiedFile`]) reads these of both slots, then copies only the one holding the log.
pub(crate) const SLOT_HEADER_BYTES: u64 = HEADER_LEN;
const TAG_PUT: u8 = 1;
const TAG_DELETE: u8 = 2;
const TAG_COMMIT: u8 = 3;
/// Tag, ID, payload length.
const PUT_HEAD: usize = 1 + LOOKUP_ID_LEN + 4;
const DELETE_LEN: usize = 1 + LOOKUP_ID_LEN;
/// Tag, frame count, CRC.
const COMMIT_LEN: usize = 1 + 4 + 4;
/// Magic, generation, slot, covered length, entry count.
const INDEX_HEAD: usize = 8 + 8 + 1 + 8 + 4;
/// ID, offset, length.
const INDEX_ENTRY: usize = LOOKUP_ID_LEN + 8 + 4;
/// How much of the log is read at a time while it is scanned or copied.
const CHUNK: usize = 1 << 20;
/// Payloads larger than this are a damaged frame, not a record (the largest records are a few MiB).
const MAX_PAYLOAD: u32 = 256 << 20;
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LONGEST_RETRY: Duration = Duration::from_secs(30);

/// The records of one store in a log and its index; see the module docs.
pub(crate) struct RecordLog<F> {
  slots: [F; 2],
  index_file: F,
  limits: LogLimits,
  /// The slot holding the log.
  active: usize,
  generation: u64,
  /// End of the last committed group in the active slot.
  end: u64,
  /// How much of the log the index file covers.
  checkpointed: u64,
  index: BTreeMap<LookupId, Place>,
  /// Bytes of the frames the index points to.
  live_bytes: u64,
  group: Group,
  /// Groups the disk refused, oldest first merged: written again with the next group.
  unsaved: BTreeMap<LookupId, Option<Vec<u8>>>,
  maybe_failure: Option<Failure>,
}

/// Sizes that decide when the log writes a checkpoint or compacts, and how many unsaved records
/// make it backlogged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LogLimits {
  pub checkpoint_bytes: u64,
  pub compact_bytes: u64,
  pub max_unsaved: usize,
}

/// The end of an appended group, and where each of its records went (`None`: deleted).
type Appended = (u64, Vec<(LookupId, Option<Place>)>);

/// What [`RecordLog::open`] found.
pub(crate) enum Opened<F> {
  Log(RecordLog<F>),
  /// No log: the files, for [`RecordLog::create`].
  Empty {
    slots: [F; 2],
    index_file: F,
  },
}

/// One file of the log: a slot or the index. Reads and writes are synchronous (OPFS sync access
/// handles in a worker, memory in tests).
pub(crate) trait LogFile {
  fn size(&self) -> Result<u64, LogFileError>;
  /// Reads up to `buffer.len()` bytes at `offset`; fewer only at the end of the file.
  fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, LogFileError>;
  fn write_at(&self, offset: u64, data: &[u8]) -> Result<(), LogFileError>;
  fn truncate(&self, size: u64) -> Result<(), LogFileError>;
  /// Makes what was written durable.
  fn flush(&self) -> Result<(), LogFileError>;
}

/// Why a file operation failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum LogFileError {
  #[error("there is not enough storage space")]
  QuotaExceeded,
  #[error("the file was closed")]
  Closed,
  #[error("file error: {0}")]
  Io(String),
}

/// A file's bytes copied into memory, for a reader that can't hold the files (the browser's
/// service worker has no sync access handles, e2ee.md §9.3). Opening a log over copies reads the
/// copies; a group cut off as incomplete is cut off the copy only, never the file; writes are
/// refused. The owner may write the files meanwhile: the copy is the log as it was when copied,
/// which is a prefix of its groups like any log.
pub(crate) struct CopiedFile {
  bytes: RefCell<Vec<u8>>,
}

/// Where a record's sealed bytes are in the active slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Place {
  offset: u64,
  len: u32,
}

/// The open group of writes: nested groups end with the outermost.
#[derive(Default)]
struct Group {
  depth: u32,
  writes: BTreeMap<LookupId, Option<Vec<u8>>>,
}

/// The last write the disk refused, and when the unsaved writes may go again.
#[derive(Clone, Debug)]
struct Failure {
  error: LogFileError,
  retry_at_ms: i64,
  backoff: Duration,
}

/// What a log slot's header says.
struct SlotHeader {
  generation: u64,
}

/// The index file's checkpoint, when it fits the log.
struct Checkpoint {
  covered: u64,
  index: BTreeMap<LookupId, Place>,
}

/// Frames read back from a log, group by group.
struct Scan {
  index: BTreeMap<LookupId, Place>,
  /// End of the last complete group.
  end: u64,
}

impl Default for LogLimits {
  fn default() -> Self {
    Self {
      checkpoint_bytes: 4 << 20,
      compact_bytes: 16 << 20,
      max_unsaved: 2000,
    }
  }
}

/// The slot holding the log in `slots`; `None` when neither does.
pub(crate) fn newest_slot<F: LogFile>(slots: &[F; 2]) -> Result<Option<usize>, LogFileError> {
  Ok(newest(&[read_header(&slots[0])?, read_header(&slots[1])?]))
}

impl<F: LogFile> RecordLog<F> {
  /// Opens the log in `slots` with `index_file`. [`Opened::Empty`]: neither slot holds a log (a new
  /// store, or one whose first write was interrupted); [`Self::create`] makes one in its files.
  pub(crate) fn open(slots: [F; 2], index_file: F, limits: LogLimits) -> Result<Opened<F>, LogFileError> {
    let headers = [read_header(&slots[0])?, read_header(&slots[1])?];
    let Some(active) = newest(&headers) else {
      return Ok(Opened::Empty { slots, index_file });
    };
    let Some(header) = &headers[active] else {
      return Ok(Opened::Empty { slots, index_file });
    };
    let generation = header.generation;
    let size = slots[active].size()?;
    let maybe_checkpoint = read_checkpoint(&index_file, generation, active, size)?;
    let (start, index) = match maybe_checkpoint {
      Some(checkpoint) => (checkpoint.covered, checkpoint.index),
      None => (HEADER_LEN, BTreeMap::new()),
    };
    let checkpointed = if start == HEADER_LEN { 0 } else { start };
    let scan = scan(&slots[active], start, size, index)?;
    if scan.end < size {
      tracing::warn!(
        dropped_bytes = size - scan.end,
        "the local store's log ended in an incomplete group; it was cut off"
      );
      slots[active].truncate(scan.end)?;
    }
    let live_bytes = scan.index.values().map(|place| put_frame_len(place.len)).sum();
    Ok(Opened::Log(Self {
      slots,
      index_file,
      limits,
      active,
      generation,
      end: scan.end,
      checkpointed,
      index: scan.index,
      live_bytes,
      group: Group::default(),
      unsaved: BTreeMap::new(),
      maybe_failure: None,
    }))
  }

  /// Makes a new log in slot 0 holding `records` (a store migrated from IndexedDB, or none): the
  /// header goes last, so an interrupted creation leaves no log and is done again.
  pub(crate) fn create(
    slots: [F; 2],
    index_file: F,
    limits: LogLimits,
    records: impl IntoIterator<Item = (LookupId, Vec<u8>)>,
  ) -> Result<Self, LogFileError> {
    for slot in &slots {
      slot.truncate(0)?;
    }
    let mut log = Self {
      slots,
      index_file,
      limits,
      active: 0,
      generation: 1,
      end: HEADER_LEN,
      checkpointed: 0,
      index: BTreeMap::new(),
      live_bytes: 0,
      group: Group::default(),
      unsaved: BTreeMap::new(),
      maybe_failure: None,
    };
    let mut writer = GroupWriter::new(&log.slots[0], 0);
    writer.write(&[0; HEADER_LEN as usize])?;
    writer.reset_group();
    for (id, sealed) in records {
      let place = writer.put(&id, &sealed)?;
      log.live_bytes += put_frame_len(place.len);
      log.index.insert(id, place);
    }
    log.end = writer.commit()?;
    log.slots[0].flush()?;
    log.slots[0].write_at(0, &encode_header(log.generation))?;
    log.slots[0].flush()?;
    log.write_checkpoint()?;
    Ok(log)
  }

  /// The record's latest bytes, including writes not on disk yet.
  pub(crate) fn get(&self, id: &LookupId) -> Result<Option<Vec<u8>>, LogFileError> {
    if let Some(write) = self.group.writes.get(id).or_else(|| self.unsaved.get(id)) {
      return Ok(write.clone());
    }
    let Some(place) = self.index.get(id) else {
      return Ok(None);
    };
    let mut sealed = vec![0; place.len as usize];
    read_exact(&self.slots[self.active], place.offset, &mut sealed)?;
    Ok(Some(sealed))
  }

  /// Up to `limit` records after `maybe_after`, in ID order, as [`Self::get`] sees them.
  pub(crate) fn records_after(
    &self,
    maybe_after: Option<&LookupId>,
    limit: usize,
  ) -> Result<Vec<(LookupId, Vec<u8>)>, LogFileError> {
    let range = |after: Option<&LookupId>| match after {
      Some(after) => (Bound::Excluded(*after), Bound::Unbounded),
      None => (Bound::Unbounded, Bound::Unbounded),
    };
    let mut overlay: BTreeMap<LookupId, Option<&Vec<u8>>> = BTreeMap::new();
    for (id, write) in self
      .unsaved
      .range(range(maybe_after))
      .chain(self.group.writes.range(range(maybe_after)))
    {
      overlay.insert(*id, write.as_ref());
    }
    let mut overlay = overlay.into_iter().peekable();
    let mut stored = self.index.range(range(maybe_after)).peekable();
    let mut batch = Vec::with_capacity(limit.min(1024));
    while batch.len() < limit {
      let take_overlay = match (overlay.peek(), stored.peek()) {
        (None, None) => break,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (Some((written, _)), Some((kept, _))) => written <= *kept,
      };
      if take_overlay {
        let Some((id, write)) = overlay.next() else {
          break;
        };
        if stored.peek().is_some_and(|(kept, _)| **kept == id) {
          stored.next();
        }
        if let Some(sealed) = write {
          batch.push((id, sealed.clone()));
        }
      } else if let Some((id, place)) = stored.next() {
        let mut sealed = vec![0; place.len as usize];
        read_exact(&self.slots[self.active], place.offset, &mut sealed)?;
        batch.push((*id, sealed));
      }
    }
    Ok(batch)
  }

  /// Puts a record; outside a group it is written as a group of its own.
  pub(crate) fn put(&mut self, id: &LookupId, sealed: &[u8], now_ms: i64) {
    self.group.writes.insert(*id, Some(sealed.to_vec()));
    if self.group.depth == 0 {
      self.commit(now_ms);
    }
  }

  /// Deletes a record; outside a group it is written as a group of its own.
  pub(crate) fn delete(&mut self, id: &LookupId, now_ms: i64) {
    self.group.writes.insert(*id, None);
    if self.group.depth == 0 {
      self.commit(now_ms);
    }
  }

  pub(crate) fn begin_group(&mut self) {
    self.group.depth = self.group.depth.saturating_add(1);
  }

  /// Ends a group; the outermost writes it as one group of the log.
  pub(crate) fn end_group(&mut self, now_ms: i64) {
    self.group.depth = self.group.depth.saturating_sub(1);
    if self.group.depth == 0 {
      self.commit(now_ms);
    }
  }

  /// Writes what the disk refused before, now (no backoff), makes the log durable and reports why
  /// writes are still unsaved, if they are.
  pub(crate) fn flush(&mut self, now_ms: i64) -> Result<(), LogFileError> {
    if self.group.depth == 0 && !self.unsaved.is_empty() {
      self.write_unsaved(now_ms);
    }
    if let Some(failure) = &self.maybe_failure {
      return Err(failure.error.clone());
    }
    self.slots[self.active].flush()
  }

  /// Writes the index file now, so the next open scans nothing: the owner calls it when the page is
  /// hidden. Does nothing when it is up to date.
  pub(crate) fn checkpoint(&mut self) -> Result<(), LogFileError> {
    if self.checkpointed == self.end {
      return Ok(());
    }
    self.slots[self.active].flush()?;
    self.write_checkpoint()
  }

  /// Whether so many records wait for a disk that refuses them that the owner should stop writing
  /// what it can fetch again later.
  pub(crate) fn is_backlogged(&self) -> bool {
    self.unsaved.len() > self.limits.max_unsaved
  }

  /// The slot holding the log.
  pub(crate) fn active_slot(&self) -> usize {
    self.active
  }

  /// Records held, and bytes of the log on disk.
  pub(crate) fn sizes(&self) -> (usize, u64) {
    (self.index.len(), self.end)
  }

  /// Writes the open group with what was unsaved, unless the backoff of a refused write runs.
  fn commit(&mut self, now_ms: i64) {
    let writes = std::mem::take(&mut self.group.writes);
    if writes.is_empty() && self.unsaved.is_empty() {
      return;
    }
    self.unsaved.extend(writes);
    if self
      .maybe_failure
      .as_ref()
      .is_some_and(|failure| now_ms < failure.retry_at_ms)
    {
      return;
    }
    self.write_unsaved(now_ms);
  }

  fn write_unsaved(&mut self, now_ms: i64) {
    match self.append(&self.unsaved) {
      Ok((end, places)) => {
        self.apply(end, places);
        self.unsaved.clear();
        self.maybe_failure = None;
        self.after_commit();
      },
      Err(error) => self.note_failure(error, now_ms),
    }
  }

  /// Appends `writes` as one group; on failure the slot is cut back to the last group.
  fn append(&self, writes: &BTreeMap<LookupId, Option<Vec<u8>>>) -> Result<Appended, LogFileError> {
    let slot = &self.slots[self.active];
    let mut writer = GroupWriter::new(slot, self.end);
    let mut places = Vec::with_capacity(writes.len());
    let written = (|| {
      for (id, write) in writes {
        match write {
          Some(sealed) => places.push((*id, Some(writer.put(id, sealed)?))),
          None => {
            writer.delete(id)?;
            places.push((*id, None));
          },
        }
      }
      writer.commit()
    })();
    match written {
      Ok(end) => Ok((end, places)),
      Err(error) => {
        // Deliberately ignored: the open cuts off an incomplete group anyway.
        let _ = slot.truncate(self.end);
        Err(error)
      },
    }
  }

  fn apply(&mut self, end: u64, places: Vec<(LookupId, Option<Place>)>) {
    for (id, maybe_place) in places {
      if let Some(old) = self.index.remove(&id) {
        self.live_bytes -= put_frame_len(old.len);
      }
      if let Some(place) = maybe_place {
        self.live_bytes += put_frame_len(place.len);
        self.index.insert(id, place);
      }
    }
    self.end = end;
  }

  fn note_failure(&mut self, error: LogFileError, now_ms: i64) {
    let backoff = self
      .maybe_failure
      .as_ref()
      .map_or(FIRST_RETRY, |failure| (failure.backoff * 2).min(LONGEST_RETRY));
    tracing::error!(
      quota = error == LogFileError::QuotaExceeded,
      unsaved = self.unsaved.len(),
      "the local store's log refused a write"
    );
    self.maybe_failure = Some(Failure {
      error,
      retry_at_ms: now_ms.saturating_add(i64::try_from(backoff.as_millis()).unwrap_or(i64::MAX)),
      backoff,
    });
  }

  /// Compacts or writes a checkpoint when due. Failures leave the log as it is: both are
  /// optimizations, tried again after the next group.
  fn after_commit(&mut self) {
    if self.is_compaction_due() {
      match self.compact() {
        Ok(()) => return,
        Err(error) => tracing::warn!(%error, "the local store's log could not be compacted"),
      }
    }
    let tail = self.end - self.checkpointed.max(HEADER_LEN);
    let index_bytes = (self.index.len() * INDEX_ENTRY) as u64;
    if tail > self.limits.checkpoint_bytes.max(index_bytes)
      && let Err(error) = self.checkpoint()
    {
      tracing::warn!(%error, "the local store's index could not be saved");
    }
  }

  fn is_compaction_due(&self) -> bool {
    self.end > self.limits.compact_bytes && self.end > 2 * (self.live_bytes + HEADER_LEN)
  }

  /// Copies the live records into the other slot as one group, then makes it the log.
  fn compact(&mut self) -> Result<(), LogFileError> {
    let target = 1 - self.active;
    let source = &self.slots[self.active];
    let slot = &self.slots[target];
    slot.truncate(0)?;
    let copied = (|| {
      let mut writer = GroupWriter::new(slot, 0);
      writer.write(&[0; HEADER_LEN as usize])?;
      writer.reset_group();
      let mut index = BTreeMap::new();
      let mut sealed = Vec::new();
      for (id, place) in &self.index {
        sealed.resize(place.len as usize, 0);
        read_exact(source, place.offset, &mut sealed)?;
        index.insert(*id, writer.put(id, &sealed)?);
      }
      let end = writer.commit()?;
      slot.flush()?;
      slot.write_at(0, &encode_header(self.generation + 1))?;
      slot.flush()?;
      Ok((end, index))
    })();
    let (end, index) = match copied {
      Ok(copied) => copied,
      Err(error) => {
        // Deliberately ignored: a slot without a valid header is never read.
        let _ = slot.truncate(0);
        return Err(error);
      },
    };
    let before = self.end;
    self.active = target;
    self.generation += 1;
    self.end = end;
    self.index = index;
    self.checkpointed = 0;
    // Only now: until the new header was on disk the old slot was the log.
    if let Err(error) = self.slots[1 - target].truncate(0) {
      tracing::warn!(%error, "the local store's old log could not be emptied");
    }
    tracing::info!(
      before_bytes = before,
      after_bytes = end,
      "compacted the local store's log"
    );
    self.write_checkpoint()
  }

  fn write_checkpoint(&mut self) -> Result<(), LogFileError> {
    let mut data = Vec::with_capacity(INDEX_HEAD + self.index.len() * INDEX_ENTRY + 4);
    data.extend_from_slice(&INDEX_MAGIC);
    data.extend_from_slice(&self.generation.to_le_bytes());
    // Invariant: there are two slots.
    data.push(self.active as u8);
    data.extend_from_slice(&self.end.to_le_bytes());
    data.extend_from_slice(&u32::try_from(self.index.len()).unwrap_or(u32::MAX).to_le_bytes());
    for (id, place) in &self.index {
      data.extend_from_slice(&id.0);
      data.extend_from_slice(&place.offset.to_le_bytes());
      data.extend_from_slice(&place.len.to_le_bytes());
    }
    let crc = crc32fast::hash(&data);
    data.extend_from_slice(&crc.to_le_bytes());
    self.index_file.write_at(0, &data)?;
    self.index_file.truncate(data.len() as u64)?;
    self.index_file.flush()?;
    self.checkpointed = self.end;
    Ok(())
  }
}

/// Appends frames to a slot through a buffer, and the group's commit frame.
struct GroupWriter<'f, F> {
  file: &'f F,
  /// Where the buffer goes.
  offset: u64,
  buffer: Vec<u8>,
  crc: crc32fast::Hasher,
  frames: u32,
}

impl<'f, F: LogFile> GroupWriter<'f, F> {
  fn new(file: &'f F, offset: u64) -> Self {
    Self {
      file,
      offset,
      buffer: Vec::new(),
      crc: crc32fast::Hasher::new(),
      frames: 0,
    }
  }

  fn put(&mut self, id: &LookupId, sealed: &[u8]) -> Result<Place, LogFileError> {
    let len = u32::try_from(sealed.len()).map_err(|_| LogFileError::Io("record too large".into()))?;
    let mut head = [0u8; PUT_HEAD];
    head[0] = TAG_PUT;
    head[1..=LOOKUP_ID_LEN].copy_from_slice(&id.0);
    head[1 + LOOKUP_ID_LEN..].copy_from_slice(&len.to_le_bytes());
    self.frame(&head)?;
    let offset = self.position();
    self.frame(sealed)?;
    self.frames += 1;
    Ok(Place { offset, len })
  }

  fn delete(&mut self, id: &LookupId) -> Result<(), LogFileError> {
    let mut frame = [0u8; DELETE_LEN];
    frame[0] = TAG_DELETE;
    frame[1..].copy_from_slice(&id.0);
    self.frame(&frame)?;
    self.frames += 1;
    Ok(())
  }

  /// Writes the commit frame and everything buffered; returns the end of the group.
  fn commit(mut self) -> Result<u64, LogFileError> {
    let crc = std::mem::take(&mut self.crc).finalize();
    let mut frame = [0u8; COMMIT_LEN];
    frame[0] = TAG_COMMIT;
    frame[1..5].copy_from_slice(&self.frames.to_le_bytes());
    frame[5..].copy_from_slice(&crc.to_le_bytes());
    self.write(&frame)?;
    self.drain()?;
    Ok(self.offset)
  }

  /// Bytes that count for the group's CRC.
  fn frame(&mut self, bytes: &[u8]) -> Result<(), LogFileError> {
    self.crc.update(bytes);
    self.write(bytes)
  }

  fn write(&mut self, bytes: &[u8]) -> Result<(), LogFileError> {
    self.buffer.extend_from_slice(bytes);
    if self.buffer.len() >= CHUNK {
      self.drain()?;
    }
    Ok(())
  }

  /// Forgets what was hashed so far: the placeholder header isn't part of a group.
  fn reset_group(&mut self) {
    self.crc = crc32fast::Hasher::new();
    self.frames = 0;
  }

  fn position(&self) -> u64 {
    self.offset + self.buffer.len() as u64
  }

  fn drain(&mut self) -> Result<(), LogFileError> {
    if self.buffer.is_empty() {
      return Ok(());
    }
    self.file.write_at(self.offset, &self.buffer)?;
    self.offset += self.buffer.len() as u64;
    self.buffer.clear();
    Ok(())
  }
}

/// Reads a slot's frames from `start` to `size` into `index`, group by group, up to the end of the
/// last complete group.
fn scan<F: LogFile>(
  file: &F,
  start: u64,
  size: u64,
  mut index: BTreeMap<LookupId, Place>,
) -> Result<Scan, LogFileError> {
  let mut reader = ChunkReader::new(file, start, size);
  let mut end = start;
  let mut group: Vec<(LookupId, Option<Place>)> = Vec::new();
  let mut crc = crc32fast::Hasher::new();
  while let Some(tag) = reader.take(1)? {
    match tag[0] {
      TAG_PUT => {
        crc.update(&tag);
        let Some(rest) = reader.take(PUT_HEAD - 1)? else {
          break;
        };
        crc.update(&rest);
        let id = lookup_id(&rest[..LOOKUP_ID_LEN]);
        let len = u32::from_le_bytes(array(&rest[LOOKUP_ID_LEN..]));
        if len > MAX_PAYLOAD {
          break;
        }
        let offset = reader.position();
        let Some(payload) = reader.take(len as usize)? else {
          break;
        };
        crc.update(&payload);
        group.push((id, Some(Place { offset, len })));
      },
      TAG_DELETE => {
        crc.update(&tag);
        let Some(rest) = reader.take(LOOKUP_ID_LEN)? else {
          break;
        };
        crc.update(&rest);
        group.push((lookup_id(&rest), None));
      },
      TAG_COMMIT => {
        let Some(rest) = reader.take(COMMIT_LEN - 1)? else {
          break;
        };
        let frames = u32::from_le_bytes(array(&rest[..4]));
        let expected = u32::from_le_bytes(array(&rest[4..]));
        let actual = std::mem::take(&mut crc).finalize();
        if expected != actual || frames as usize != group.len() {
          break;
        }
        for (id, maybe_place) in group.drain(..) {
          match maybe_place {
            Some(place) => index.insert(id, place),
            None => index.remove(&id),
          };
        }
        end = reader.position();
      },
      _ => break,
    }
  }
  Ok(Scan { index, end })
}

/// Sequential reads of a file in chunks of [`CHUNK`].
struct ChunkReader<'f, F> {
  file: &'f F,
  /// File offset of `buffer[0]`.
  base: u64,
  buffer: Vec<u8>,
  cursor: usize,
  size: u64,
}

impl<'f, F: LogFile> ChunkReader<'f, F> {
  fn new(file: &'f F, start: u64, size: u64) -> Self {
    Self {
      file,
      base: start,
      buffer: Vec::new(),
      cursor: 0,
      size,
    }
  }

  fn position(&self) -> u64 {
    self.base + self.cursor as u64
  }

  /// The next `len` bytes; `None` when the file ends before.
  fn take(&mut self, len: usize) -> Result<Option<Vec<u8>>, LogFileError> {
    if self.position() + len as u64 > self.size {
      return Ok(None);
    }
    if self.cursor + len > self.buffer.len() {
      let start = self.position();
      let want = (self.size - start).min(CHUNK.max(len) as u64) as usize;
      self.buffer.resize(want, 0);
      read_exact(self.file, start, &mut self.buffer)?;
      self.base = start;
      self.cursor = 0;
    }
    let taken = self.buffer[self.cursor..self.cursor + len].to_vec();
    self.cursor += len;
    Ok(Some(taken))
  }
}

impl CopiedFile {
  pub(crate) fn new(bytes: Vec<u8>) -> Self {
    Self {
      bytes: RefCell::new(bytes),
    }
  }
}

impl LogFile for CopiedFile {
  fn size(&self) -> Result<u64, LogFileError> {
    Ok(self.bytes.borrow().len() as u64)
  }

  fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, LogFileError> {
    let bytes = self.bytes.borrow();
    let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
    let len = buffer.len().min(bytes.len() - start);
    buffer[..len].copy_from_slice(&bytes[start..start + len]);
    Ok(len)
  }

  fn write_at(&self, _offset: u64, _data: &[u8]) -> Result<(), LogFileError> {
    Err(LogFileError::Io("a copy of the log is read-only".into()))
  }

  /// Cuts the copy: what opening does to a log whose last group is incomplete.
  fn truncate(&self, size: u64) -> Result<(), LogFileError> {
    let mut bytes = self.bytes.borrow_mut();
    let size = usize::try_from(size).unwrap_or(usize::MAX).min(bytes.len());
    bytes.truncate(size);
    Ok(())
  }

  fn flush(&self) -> Result<(), LogFileError> {
    Ok(())
  }
}

/// The slot whose header is valid, the newer generation if both are.
fn newest(headers: &[Option<SlotHeader>; 2]) -> Option<usize> {
  match (&headers[0], &headers[1]) {
    (None, None) => None,
    (Some(_), None) => Some(0),
    (None, Some(_)) => Some(1),
    (Some(first), Some(second)) => Some(usize::from(second.generation > first.generation)),
  }
}

fn read_header<F: LogFile>(file: &F) -> Result<Option<SlotHeader>, LogFileError> {
  let mut header = [0u8; HEADER_LEN as usize];
  if file.read_at(0, &mut header)? < header.len() {
    return Ok(None);
  }
  let crc = u32::from_le_bytes(array(&header[16..]));
  if header[..8] != LOG_MAGIC || crc32fast::hash(&header[..16]) != crc {
    return Ok(None);
  }
  Ok(Some(SlotHeader {
    generation: u64::from_le_bytes(array(&header[8..16])),
  }))
}

fn encode_header(generation: u64) -> [u8; HEADER_LEN as usize] {
  let mut header = [0u8; HEADER_LEN as usize];
  header[..8].copy_from_slice(&LOG_MAGIC);
  header[8..16].copy_from_slice(&generation.to_le_bytes());
  let crc = crc32fast::hash(&header[..16]);
  header[16..].copy_from_slice(&crc.to_le_bytes());
  header
}

/// The index file's checkpoint, if it is intact and belongs to this log (`generation` in `slot`,
/// covering no more than the slot's `size`).
fn read_checkpoint<F: LogFile>(
  file: &F,
  generation: u64,
  slot: usize,
  size: u64,
) -> Result<Option<Checkpoint>, LogFileError> {
  let length = file.size()?;
  if length < (INDEX_HEAD + 4) as u64 {
    return Ok(None);
  }
  let Ok(length) = usize::try_from(length) else {
    return Ok(None);
  };
  let mut data = vec![0u8; length];
  read_exact(file, 0, &mut data)?;
  let (body, crc) = data.split_at(length - 4);
  if crc32fast::hash(body) != u32::from_le_bytes(array(crc)) || body[..8] != INDEX_MAGIC {
    return Ok(None);
  }
  let covered = u64::from_le_bytes(array(&body[17..25]));
  let count = u32::from_le_bytes(array(&body[25..29])) as usize;
  let is_ours = u64::from_le_bytes(array(&body[8..16])) == generation && usize::from(body[16]) == slot;
  if !is_ours || covered > size || covered < HEADER_LEN || body.len() != INDEX_HEAD + count * INDEX_ENTRY {
    return Ok(None);
  }
  let mut index = BTreeMap::new();
  for entry in body[INDEX_HEAD..].as_chunks::<INDEX_ENTRY>().0 {
    let id = lookup_id(&entry[..LOOKUP_ID_LEN]);
    let offset = u64::from_le_bytes(array(&entry[LOOKUP_ID_LEN..LOOKUP_ID_LEN + 8]));
    let len = u32::from_le_bytes(array(&entry[LOOKUP_ID_LEN + 8..]));
    // A place outside the covered log would read garbage or allocate a damaged length: the
    // checkpoint is only a hint, so the log is scanned instead.
    let is_inside = offset
      .checked_add(u64::from(len))
      .is_some_and(|end| end <= covered);
    if len > MAX_PAYLOAD || offset < HEADER_LEN || !is_inside {
      tracing::warn!("the local store's index points outside its log; scanning the log instead");
      return Ok(None);
    }
    index.insert(id, Place { offset, len });
  }
  Ok(Some(Checkpoint { covered, index }))
}

fn read_exact<F: LogFile>(file: &F, offset: u64, buffer: &mut [u8]) -> Result<(), LogFileError> {
  let mut read = 0;
  while read < buffer.len() {
    let got = file.read_at(offset + read as u64, &mut buffer[read..])?;
    if got == 0 {
      return Err(LogFileError::Io("the file ended early".into()));
    }
    read += got;
  }
  Ok(())
}

fn put_frame_len(len: u32) -> u64 {
  PUT_HEAD as u64 + u64::from(len)
}

fn lookup_id(bytes: &[u8]) -> LookupId {
  LookupId(array(bytes))
}

/// The first `N` bytes of `bytes`; callers pass slices of exactly that length.
fn array<const N: usize>(bytes: &[u8]) -> [u8; N] {
  let mut array = [0u8; N];
  array.copy_from_slice(&bytes[..N]);
  array
}

#[cfg(test)]
mod tests {
  use std::rc::Rc;

  use super::*;

  const NOW: i64 = 1_000_000;

  #[test]
  fn records_survive_reopening() {
    let files = Files::default();
    let mut log = files.create(vec![(id(1), vec![1; 10])]);
    log.put(&id(2), &[2; 20], NOW);
    log.delete(&id(1), NOW);
    drop(log);
    let log = files.open().unwrap();
    assert_eq!(log.get(&id(1)).unwrap(), None);
    assert_eq!(log.get(&id(2)).unwrap(), Some(vec![2; 20]));
  }

  #[test]
  fn a_group_reaches_the_disk_whole_or_not_at_all() {
    let files = Files::default();
    let mut log = files.create(Vec::new());
    log.begin_group();
    log.put(&id(1), &[1; 10], NOW);
    log.put(&id(2), &[2; 10], NOW);
    assert_eq!(files.slot(0).len() as u64, HEADER_LEN + COMMIT_LEN as u64);
    log.end_group(NOW);
    let complete = files.slot(0).len();
    log.put(&id(3), &[3; 10], NOW);
    drop(log);
    // The browser died within the last group: every cut of it is dropped, the first group stays.
    for cut in complete..files.slot(0).len() {
      let torn = files.copy();
      torn.slots[0].borrow_mut().truncate(cut);
      let log = torn.open().unwrap();
      assert_eq!(log.get(&id(1)).unwrap(), Some(vec![1; 10]));
      assert_eq!(log.get(&id(3)).unwrap(), None, "cut at {cut}");
      assert_eq!(torn.slot(0).len(), complete);
    }
  }

  #[test]
  fn a_damaged_group_is_cut_off() {
    let files = Files::default();
    let mut log = files.create(Vec::new());
    log.put(&id(1), &[1; 10], NOW);
    let complete = files.slot(0).len();
    log.put(&id(2), &[2; 10], NOW);
    drop(log);
    let last = files.slot(0).len() - COMMIT_LEN - 1;
    files.slots[0].borrow_mut()[last] ^= 1;
    let log = files.open().unwrap();
    assert_eq!(log.get(&id(2)).unwrap(), None);
    assert_eq!(files.slot(0).len(), complete);
  }

  #[test]
  fn an_interrupted_creation_leaves_no_log() {
    let files = Files::default();
    drop(files.create(vec![(id(1), vec![1; 10])]));
    // As if the header had not reached the disk.
    files.slots[0].borrow_mut()[..HEADER_LEN as usize].fill(0);
    assert!(files.open().is_none());
  }

  mod checkpoint {
    use super::*;

    #[test]
    fn opening_reads_the_index_and_only_the_tail() {
      let files = Files::default();
      let mut log = files.create((0..100).map(|n| (id(n), vec![n; 100])).collect());
      log.put(&id(200), &[7; 5], NOW);
      drop(log);
      let log = files.open_counting();
      // The checkpoint, and the tail's one chunk; no record of the first hundred.
      assert_eq!(files.reads(0), 1);
      assert_eq!(log.get(&id(50)).unwrap(), Some(vec![50; 100]));
      assert_eq!(log.get(&id(200)).unwrap(), Some(vec![7; 5]));
    }

    #[test]
    fn a_damaged_index_costs_a_scan_only() {
      let files = Files::default();
      let mut log = files.create((0..10).map(|n| (id(n), vec![n; 10])).collect());
      log.delete(&id(3), NOW);
      log.checkpoint().unwrap();
      drop(log);
      files.index.borrow_mut()[INDEX_HEAD + 3] ^= 1;
      let log = files.open().unwrap();
      assert_eq!(log.get(&id(3)).unwrap(), None);
      assert_eq!(log.get(&id(4)).unwrap(), Some(vec![4; 10]));
    }

    /// A forged entry with a valid CRC (a damaged writer, a hostile copy) never makes the store
    /// read or allocate what it says: the open scans the log instead.
    #[test]
    fn an_index_entry_outside_the_log_costs_a_scan_only() {
      let forgeries: [(Option<u64>, Option<u32>); 3] =
        [(None, Some(u32::MAX)), (Some(0), None), (Some(1 << 40), None)];
      for (maybe_offset, maybe_len) in forgeries {
        let files = Files::default();
        drop(files.create((0..10).map(|n| (id(n), vec![n; 10])).collect()));
        forge_first_entry(&files, maybe_offset, maybe_len);
        *files.reads.borrow_mut() = [0; 3];
        let log = files.open().unwrap();
        assert!(files.reads(0) > 0, "the log was scanned");
        assert_eq!(log.get(&id(0)).unwrap(), Some(vec![0; 10]));
        assert_eq!(log.sizes().0, 10);
      }
    }

    /// Rewrites the index file's first entry and its CRC.
    fn forge_first_entry(files: &Files, maybe_offset: Option<u64>, maybe_len: Option<u32>) {
      let mut index = files.index.borrow_mut();
      let entry = INDEX_HEAD + LOOKUP_ID_LEN;
      if let Some(offset) = maybe_offset {
        index[entry..entry + 8].copy_from_slice(&offset.to_le_bytes());
      }
      if let Some(len) = maybe_len {
        index[entry + 8..entry + 12].copy_from_slice(&len.to_le_bytes());
      }
      let body = index.len() - 4;
      let crc = crc32fast::hash(&index[..body]);
      index[body..].copy_from_slice(&crc.to_le_bytes());
    }

    #[test]
    fn an_index_of_a_longer_log_is_ignored() {
      let files = Files::default();
      let mut log = files.create(Vec::new());
      log.put(&id(1), &[1; 10], NOW);
      log.checkpoint().unwrap();
      drop(log);
      // The disk lost the log's end after the index was written (it isn't, but a copy could be).
      files.slots[0].borrow_mut().truncate(HEADER_LEN as usize);
      let log = files.open().unwrap();
      assert_eq!(log.get(&id(1)).unwrap(), None);
    }

    #[test]
    fn is_written_once_the_tail_outgrows_it() {
      let files = Files::default();
      let mut log = files.create_with(Vec::new(), limits(100, u64::MAX));
      log.put(&id(1), &[1; 50], NOW);
      assert!(log.checkpointed < log.end);
      log.put(&id(2), &[2; 50], NOW);
      assert_eq!(log.checkpointed, log.end);
    }
  }

  mod compaction {
    use super::*;

    #[test]
    fn moves_the_live_records_to_the_other_slot() {
      let files = Files::default();
      let mut log = files.create_with(Vec::new(), limits(u64::MAX, 1000));
      for round in 0..20 {
        log.put(&id(1), &[round; 100], NOW);
      }
      log.put(&id(2), &[9; 10], NOW);
      assert!(log.generation > 1);
      assert!(log.end < 1000);
      assert!(files.slot(1 - log.active).is_empty());
      drop(log);
      let log = files.open().unwrap();
      assert_eq!(log.get(&id(1)).unwrap(), Some(vec![19; 100]));
      assert_eq!(log.get(&id(2)).unwrap(), Some(vec![9; 10]));
    }

    #[test]
    fn interrupted_before_the_new_header_keeps_the_old_log() {
      let files = Files::default();
      let mut log = files.create(vec![(id(1), vec![1; 10])]);
      log.put(&id(2), &[2; 10], NOW);
      drop(log);
      // A copy without its header in the other slot.
      let partial = files.slot(0);
      files.slots[1]
        .borrow_mut()
        .extend_from_slice(&[0; HEADER_LEN as usize]);
      files.slots[1]
        .borrow_mut()
        .extend_from_slice(&partial[HEADER_LEN as usize..]);
      let log = files.open().unwrap();
      assert_eq!(log.active, 0);
      assert_eq!(log.get(&id(2)).unwrap(), Some(vec![2; 10]));
    }

    #[test]
    fn the_newer_generation_wins_over_a_leftover_slot() {
      let files = Files::default();
      let mut log = files.create_with(vec![(id(1), vec![0; 10])], limits(u64::MAX, 200));
      let mut round = 0;
      while log.active == 0 {
        round += 1;
        log.put(&id(1), &[round; 40], NOW);
      }
      drop(log);
      // As if emptying the old slot hadn't reached the disk.
      let old = Files::default();
      drop(old.create(vec![(id(1), vec![0; 10])]));
      *files.slots[0].borrow_mut() = old.slot(0);
      let log = files.open().unwrap();
      assert_eq!(log.get(&id(1)).unwrap(), Some(vec![round; 40]));
    }
  }

  mod refused_writes {
    use super::*;

    #[test]
    fn are_kept_read_and_written_again_in_order() {
      let files = Files::default();
      let mut log = files.create(Vec::new());
      files.refuse_writes(true);
      log.put(&id(1), &[1; 10], NOW);
      log.delete(&id(1), NOW);
      log.put(&id(2), &[2; 10], NOW);
      assert_eq!(log.get(&id(2)).unwrap(), Some(vec![2; 10]));
      assert_eq!(log.flush(NOW), Err(LogFileError::QuotaExceeded));
      assert_eq!(files.slot(0).len() as u64, HEADER_LEN + COMMIT_LEN as u64);
      files.refuse_writes(false);
      // Within the backoff: kept in memory still.
      log.put(&id(3), &[3; 10], NOW + 10);
      assert_eq!(files.slot(0).len() as u64, HEADER_LEN + COMMIT_LEN as u64);
      log.put(&id(4), &[4; 10], NOW + 5000);
      assert_eq!(log.flush(NOW + 5000), Ok(()));
      drop(log);
      let log = files.open().unwrap();
      assert_eq!(log.get(&id(1)).unwrap(), None);
      assert_eq!(log.get(&id(3)).unwrap(), Some(vec![3; 10]));
      assert_eq!(log.get(&id(4)).unwrap(), Some(vec![4; 10]));
    }

    #[test]
    fn back_off_up_to_the_longest_wait() {
      let files = Files::default();
      let mut log = files.create(Vec::new());
      files.refuse_writes(true);
      let mut now = NOW;
      for _ in 0..10 {
        log.put(&id(1), &[1; 10], now);
        now = log.maybe_failure.as_ref().unwrap().retry_at_ms;
      }
      assert_eq!(log.maybe_failure.unwrap().backoff, LONGEST_RETRY);
    }

    #[test]
    fn past_the_limit_make_the_log_backlogged() {
      let files = Files::default();
      let mut log = files.create_with(
        Vec::new(),
        LogLimits {
          max_unsaved: 2,
          ..LogLimits::default()
        },
      );
      files.refuse_writes(true);
      for n in 0..3 {
        log.put(&id(n), &[n; 10], NOW);
      }
      assert!(log.is_backlogged());
      files.refuse_writes(false);
      log.flush(NOW).unwrap();
      assert!(!log.is_backlogged());
    }
  }

  #[test]
  fn records_after_merges_disk_and_memory() {
    let files = Files::default();
    let mut log = files.create((1..=5).map(|n| (id(n), vec![n; 3])).collect());
    log.begin_group();
    log.delete(&id(2), NOW);
    log.put(&id(3), &[30; 3], NOW);
    log.put(&id(6), &[6; 3], NOW);
    let all = log.records_after(None, 100).unwrap();
    let ids: Vec<u8> = all.iter().map(|(id, _)| id.0[0]).collect();
    assert_eq!(ids, vec![1, 3, 4, 5, 6]);
    assert_eq!(all[1].1, vec![30; 3]);
    let page = log.records_after(Some(&id(3)), 2).unwrap();
    assert_eq!(page.iter().map(|(id, _)| id.0[0]).collect::<Vec<_>>(), vec![4, 5]);
    log.end_group(NOW);
    assert_eq!(log.get(&id(2)).unwrap(), None);
    assert_eq!(log.get(&id(3)).unwrap(), Some(vec![30; 3]));
  }

  #[test]
  fn hostile_frames_are_not_trusted() {
    let files = Files::default();
    drop(files.create(Vec::new()));
    // A put claiming 4 GiB, and garbage.
    let mut frame = vec![TAG_PUT];
    frame.extend_from_slice(&[0; LOOKUP_ID_LEN]);
    frame.extend_from_slice(&u32::MAX.to_le_bytes());
    files.slots[0].borrow_mut().extend_from_slice(&frame);
    let log = files.open().unwrap();
    assert_eq!(log.sizes().0, 0);
    files.slots[0].borrow_mut().extend_from_slice(&[0xff; 100]);
    assert_eq!(files.open().unwrap().sizes().0, 0);
  }

  mod copies {
    use super::*;

    /// The service worker reads a copy of the files while the owner may be writing: a copy taken
    /// in the middle of a group reads the groups before it, and cutting the incomplete group off
    /// changes the copy only.
    #[test]
    fn a_copy_taken_mid_group_reads_the_complete_groups() {
      let files = Files::default();
      let mut log = files.create(vec![(id(1), vec![1; 10])]);
      log.put(&id(2), &[2; 20], NOW);
      let complete = files.slot(0).len();
      log.put(&id(3), &[3; 30], NOW);
      let mid_group = files.slot(0)[..complete + 7].to_vec();
      drop(log);

      let log = open_copies(mid_group, files.slot(1), files.index.borrow().clone());
      assert_eq!(log.get(&id(2)).unwrap(), Some(vec![2; 20]));
      assert_eq!(log.get(&id(3)).unwrap(), None);
      assert_eq!(files.open().unwrap().get(&id(3)).unwrap(), Some(vec![3; 30]));
    }

    #[test]
    fn a_copy_refuses_writes() {
      let files = Files::default();
      let mut log = files.create(vec![(id(1), vec![1; 10])]);
      log.flush(NOW).unwrap();
      drop(log);
      let mut copy = open_copies(files.slot(0), files.slot(1), files.index.borrow().clone());
      copy.put(&id(2), &[2; 20], NOW);
      assert!(copy.flush(NOW).is_err());
      assert_eq!(files.open().unwrap().get(&id(2)).unwrap(), None);
    }

    /// The reader copies the whole of the slot the headers name, and only the header of the other.
    #[test]
    fn the_headers_name_the_slot_to_copy() {
      let files = Files::default();
      let mut log = files.create_with(Vec::new(), limits(u64::MAX, 64));
      for round in 0..20u8 {
        log.put(&id(1), &[round; 100], NOW);
      }
      drop(log);
      let headers = |slot: usize| {
        let bytes = files.slot(slot);
        CopiedFile::new(bytes[..bytes.len().min(SLOT_HEADER_BYTES as usize)].to_vec())
      };
      let active = newest_slot(&[headers(0), headers(1)]).unwrap().unwrap();
      assert!(files.slot(1 - active).len() < files.slot(active).len());
      let log = match active {
        0 => open_copies(files.slot(0), Vec::new(), files.index.borrow().clone()),
        _ => open_copies(Vec::new(), files.slot(1), files.index.borrow().clone()),
      };
      assert_eq!(log.get(&id(1)).unwrap(), Some(vec![19; 100]));
    }

    #[test]
    fn no_header_names_no_slot() {
      let empty = || CopiedFile::new(Vec::new());
      assert_eq!(newest_slot(&[empty(), empty()]).unwrap(), None);
    }

    fn open_copies(first: Vec<u8>, second: Vec<u8>, index: Vec<u8>) -> RecordLog<CopiedFile> {
      let slots = [CopiedFile::new(first), CopiedFile::new(second)];
      match RecordLog::open(slots, CopiedFile::new(index), LogLimits::default()).unwrap() {
        Opened::Log(log) => log,
        Opened::Empty { .. } => panic!("the copy holds no log"),
      }
    }
  }

  fn id(n: u8) -> LookupId {
    let mut id = [0u8; LOOKUP_ID_LEN];
    id[0] = n;
    LookupId(id)
  }

  fn limits(checkpoint_bytes: u64, compact_bytes: u64) -> LogLimits {
    LogLimits {
      checkpoint_bytes,
      compact_bytes,
      ..LogLimits::default()
    }
  }

  /// Two slots and an index in memory, shared with the logs opened on them.
  #[derive(Default)]
  struct Files {
    slots: [Rc<RefCell<Vec<u8>>>; 2],
    index: Rc<RefCell<Vec<u8>>>,
    is_refusing: Rc<std::cell::Cell<bool>>,
    reads: Rc<RefCell<[usize; 3]>>,
  }

  /// One of [`Files`].
  struct MemoryFile {
    bytes: Rc<RefCell<Vec<u8>>>,
    is_refusing: Rc<std::cell::Cell<bool>>,
    reads: Rc<RefCell<[usize; 3]>>,
    number: usize,
  }

  impl Files {
    fn create(&self, records: Vec<(LookupId, Vec<u8>)>) -> RecordLog<MemoryFile> {
      self.create_with(records, LogLimits::default())
    }

    fn create_with(&self, records: Vec<(LookupId, Vec<u8>)>, limits: LogLimits) -> RecordLog<MemoryFile> {
      let (slots, index) = self.files();
      RecordLog::create(slots, index, limits, records).unwrap()
    }

    fn open(&self) -> Option<RecordLog<MemoryFile>> {
      let (slots, index) = self.files();
      match RecordLog::open(slots, index, LogLimits::default()).unwrap() {
        Opened::Log(log) => Some(log),
        Opened::Empty { .. } => None,
      }
    }

    /// Opens with the reads counted from here.
    fn open_counting(&self) -> RecordLog<MemoryFile> {
      *self.reads.borrow_mut() = [0; 3];
      self.open().unwrap()
    }

    /// Reads of the log in slot `slot` since [`Self::open_counting`], past its header.
    fn reads(&self, slot: usize) -> usize {
      self.reads.borrow()[slot]
    }

    fn slot(&self, slot: usize) -> Vec<u8> {
      self.slots[slot].borrow().clone()
    }

    fn copy(&self) -> Files {
      Files {
        slots: [
          Rc::new(RefCell::new(self.slot(0))),
          Rc::new(RefCell::new(self.slot(1))),
        ],
        index: Rc::new(RefCell::new(self.index.borrow().clone())),
        ..Files::default()
      }
    }

    fn refuse_writes(&self, is_refusing: bool) {
      self.is_refusing.set(is_refusing);
    }

    fn files(&self) -> ([MemoryFile; 2], MemoryFile) {
      let file = |bytes: &Rc<RefCell<Vec<u8>>>, number| MemoryFile {
        bytes: bytes.clone(),
        is_refusing: self.is_refusing.clone(),
        reads: self.reads.clone(),
        number,
      };
      (
        [file(&self.slots[0], 0), file(&self.slots[1], 1)],
        file(&self.index, 2),
      )
    }
  }

  impl LogFile for MemoryFile {
    fn size(&self) -> Result<u64, LogFileError> {
      Ok(self.bytes.borrow().len() as u64)
    }

    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, LogFileError> {
      if offset > 0 {
        self.reads.borrow_mut()[self.number] += 1;
      }
      let bytes = self.bytes.borrow();
      let start = (offset as usize).min(bytes.len());
      let len = buffer.len().min(bytes.len() - start);
      buffer[..len].copy_from_slice(&bytes[start..start + len]);
      Ok(len)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> Result<(), LogFileError> {
      if self.is_refusing.get() && self.number < 2 {
        // Half of it, as a full disk might.
        let mut bytes = self.bytes.borrow_mut();
        let end = offset as usize + data.len() / 2;
        if bytes.len() < end {
          bytes.resize(end, 0);
        }
        bytes[offset as usize..end].copy_from_slice(&data[..data.len() / 2]);
        return Err(LogFileError::QuotaExceeded);
      }
      let mut bytes = self.bytes.borrow_mut();
      let end = offset as usize + data.len();
      if bytes.len() < end {
        bytes.resize(end, 0);
      }
      bytes[offset as usize..end].copy_from_slice(data);
      Ok(())
    }

    fn truncate(&self, size: u64) -> Result<(), LogFileError> {
      self.bytes.borrow_mut().resize(size as usize, 0);
      Ok(())
    }

    fn flush(&self) -> Result<(), LogFileError> {
      Ok(())
    }
  }
}
