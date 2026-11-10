//! The purge of the write-ahead log after a deleted key (docs/e2ee.md §4.3.1) while a read thread
//! holds a snapshot. It can't truncate the log then, and 1.0.29 tried again at the end of every
//! group of writes, each time waiting the connection's busy timeout (a second) under the writer's
//! lock: every write and every read that needed the writer queued behind it (#1657, #1678). Since
//! #1684 the purge runs on a connection and thread of its own when the readers are idle, and holds
//! new groups of writes back once it is overdue.
#![allow(clippy::print_stderr, reason = "the turns are printed to compare runs")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use teamcraft_core::store::{LocalStore, LogPurgeLimits, SealedSqliteStore, StoreError, Table, write_group};

use super::{PHONE_SYNC_DELAY, SlowStore, Turn, measure};

/// Groups of writes while the read holds its snapshot.
const GROUPS: u8 = 8;
/// The longest a turn of the writer may take, ten times [`PHONE_SYNC_DELAY`]: a group with one try of the purge (its syncs) and a
/// debug build on a busy machine. 1.0.29 waited the busy timeout, a second, in each.
const MAX_TURN: Duration = Duration::from_millis(500);
/// Longer than a purge that can run takes on this disk: a few syncs.
const PURGE_WAIT: Duration = Duration::from_secs(10);
/// The purge's maximum delay in the scenario where readers never go idle.
const MAX_DELAY: Duration = Duration::from_millis(500);
/// What the purge may take past [`MAX_DELAY`]: the reads open at the deadline end at once, then a
/// few checkpoints of two or three syncs each, in a debug build on a busy machine.
const BOUND_SLACK: Duration = Duration::from_secs(2);
/// The longest a group of writes may wait for an overdue purge: the checkpoints of
/// [`BOUND_SLACK`].
const MAX_HELD_TURN: Duration = BOUND_SLACK;

#[test]
fn a_purge_that_a_read_holds_up_does_not_hold_up_the_writer() {
  let store = SlowStore::open(PHONE_SYNC_DELAY);
  seed(&store);
  settle(&store);
  // The query lane is in the middle of a long read: its snapshot keeps the log's frames.
  pin(&store.second_reader);

  let mut turns = vec![delete_key(&store)];
  assert_log_kept(&store);
  for index in 0..GROUPS {
    // The engine's group of writes (a catch-up), and the main lane's own action between them.
    turns.push(measure(&store.disk, || put_in_group(&store.writer, index)));
    turns.push(measure(&store.disk, || {
      store.lane.put(Table::Account, "action", &[index; 32])
    }));
  }
  store.second_reader.end_reads();
  // Once the read is over the purge finishes by itself: the deleted key's pages leave the log.
  let started = Instant::now();
  assert!(store.writer.wait_for_purge(PURGE_WAIT), "the purge didn't finish");
  let finish = started.elapsed();

  eprintln!("turns: {turns:?}; finishing the purge: {finish:?}");
  assert!(
    finish < MAX_TURN,
    "the purge took {finish:?} after the read ended: it should run at once"
  );
  assert_turns_bounded(&turns);
  let syncs: u64 = turns.iter().map(|turn| turn.syncs.syncs()).sum();
  assert_eq!(
    syncs, 0,
    "the writer's turns synced: the purge ran on its connection, or while the read held the log: {turns:?}"
  );
  assert_log_truncated(&store);
}

#[test]
fn reads_that_move_on_do_not_make_every_group_sync() {
  let store = SlowStore::open(PHONE_SYNC_DELAY);
  seed(&store);
  pin(&store.second_reader);
  let deletion = delete_key(&store);
  assert_log_kept(&store);

  let mut turns = Vec::new();
  for index in 0..GROUPS {
    // The lane reads again and sees the newest commit, as reads do all the time: the purge tries
    // when the reads are idle, writes the new frames back and syncs, on its own connection.
    store.second_reader.end_reads();
    pin(&store.second_reader);
    turns.push(measure(&store.disk, || put_in_group(&store.writer, index)));
  }
  store.second_reader.end_reads();

  eprintln!("deletion: {deletion:?}; groups: {turns:?}");
  assert_turns_bounded(&turns);
  assert_turns_bounded(&[deletion]);
  assert!(store.writer.wait_for_purge(PURGE_WAIT), "the purge didn't finish");
}

/// Issue #1684, the security bound: two read threads read without a pause, so that one of them
/// always holds a snapshot, and the engine writes all the while. The purge never finds the readers
/// idle; past its maximum delay it holds new groups of writes back, and finishes once the reads
/// open at that moment are over. No read waits for it.
#[test]
fn a_purge_finishes_within_its_bound_while_readers_keep_reading() {
  let store = SlowStore::open(PHONE_SYNC_DELAY);
  seed(&store);
  assert!(store.writer.wait_for_purge(PURGE_WAIT), "the purge at opening");
  store.writer.set_log_purge_limits(LogPurgeLimits {
    max_delay: MAX_DELAY,
    ..LogPurgeLimits::default()
  });
  let is_done = AtomicBool::new(false);

  let (purge_took, reads, writes) = thread::scope(|scope| {
    let readers = scope.spawn(|| keep_reading(&store, &is_done));
    let writer = scope.spawn(|| keep_writing(&store, &is_done));
    let started = Instant::now();
    delete_key(&store);
    let is_purged = store.writer.wait_for_purge(MAX_DELAY + BOUND_SLACK);
    let purge_took = started.elapsed();
    is_done.store(true, Ordering::Release);
    assert!(is_purged, "no purge within {:?}", MAX_DELAY + BOUND_SLACK);
    (purge_took, readers.join().unwrap(), writer.join().unwrap())
  });

  let longest_read = reads.iter().max().copied().unwrap_or_default();
  let longest_write = writes.iter().max().copied().unwrap_or_default();
  eprintln!(
    "purge: {purge_took:?}; {} reads, longest {longest_read:?}; {} groups, longest {longest_write:?}",
    reads.len(),
    writes.len()
  );
  assert!(purge_took <= MAX_DELAY + BOUND_SLACK);
  assert!(longest_read < MAX_TURN, "a read waited {longest_read:?}");
  assert!(
    longest_write < MAX_HELD_TURN,
    "a group of writes waited {longest_write:?}"
  );
}

/// Issue #1681: SQLite's automatic checkpoint ran inside the commit that made the log long, two
/// syncs under the writer's lock. The purge's thread writes the log back instead.
#[test]
fn a_long_log_is_written_back_without_holding_the_writer() {
  let store = SlowStore::open(PHONE_SYNC_DELAY);
  seed(&store);
  settle(&store);
  let mut turns = Vec::new();
  // 64 groups of 64 KiB: the log passes SQLite's 1000 pages several times.
  for index in 0..64_u8 {
    turns.push(measure(&store.disk, || {
      write_group(&*store.writer, || {
        store.writer.put(
          Table::MlsStorage,
          &format!("large {}", index % 4),
          &[index; 64 * 1024],
        )
      })
    }));
  }
  let database_syncs: u64 = turns.iter().map(|turn| turn.syncs.database_syncs).sum();
  eprintln!("64 large groups: {turns:?}");
  // A checkpoint syncs the database file; a commit never does. The log's header is synced once by
  // the first commit after the log was restarted, which is no checkpoint.
  assert_eq!(database_syncs, 0, "a checkpoint ran inside a commit: {turns:?}");
  assert_turns_bounded(&turns);
}

/// Reads on the two read connections in turn, the next always before the last ends, until
/// `is_done`; how long each took.
fn keep_reading(store: &SlowStore, is_done: &AtomicBool) -> Vec<Duration> {
  let mut took = Vec::new();
  let mut read = |reader: &dyn LocalStore| {
    let started = Instant::now();
    reader.begin_reads().unwrap();
    reader.get(Table::MlsStorage, "entry 0").unwrap();
    took.push(started.elapsed());
  };
  read(&store.lane);
  while !is_done.load(Ordering::Acquire) {
    read(&*store.second_reader);
    store.lane.end_reads();
    read(&store.lane);
    store.second_reader.end_reads();
  }
  store.lane.end_reads();
  took
}

/// Groups of writes one after the other until `is_done`; how long each took.
fn keep_writing(store: &SlowStore, is_done: &AtomicBool) -> Vec<Duration> {
  let mut took = Vec::new();
  let mut index = 0_u8;
  while !is_done.load(Ordering::Acquire) {
    let started = Instant::now();
    put_in_group(&store.writer, index).unwrap();
    took.push(started.elapsed());
    index = (index + 1) % GROUPS;
  }
  took
}

/// Waits for the purge at opening, then commits once: the first commit after the log was
/// truncated syncs the log's header, which is no part of what the scenarios measure.
fn settle(store: &SlowStore) {
  assert!(store.writer.wait_for_purge(PURGE_WAIT), "the purge at opening");
  store.writer.put(Table::Account, "settled", &[1]).unwrap();
}

fn seed(store: &SlowStore) {
  write_group(&*store.writer, || -> Result<(), StoreError> {
    store.writer.put(Table::MlsStorage, "old key", &[1; 64])?;
    for index in 0..GROUPS {
      store
        .writer
        .put(Table::MlsStorage, &format!("entry {index}"), &[index; 64])?;
    }
    Ok(())
  })
  .unwrap();
}

/// Begins a read on `reader` and reads, so that it holds a snapshot until `end_reads`.
fn pin(reader: &SealedSqliteStore) {
  reader.begin_reads().unwrap();
  reader.get(Table::MlsStorage, "entry 0").unwrap();
}

/// Deletes a key and asks for the purge at once, as after a used key package (`client::key_packages`).
fn delete_key(store: &SlowStore) -> Turn {
  measure(&store.disk, || {
    store.writer.delete(Table::MlsStorage, "old key")?;
    store.writer.purge_freed()
  })
}

fn put_in_group(writer: &SealedSqliteStore, index: u8) -> Result<(), StoreError> {
  write_group(writer, || {
    writer.put(Table::MlsStorage, &format!("entry {index}"), &[index; 64])
  })
}

fn assert_turns_bounded(turns: &[Turn]) {
  let Some(longest) = turns.iter().max_by_key(|turn| turn.took) else {
    return;
  };
  assert!(
    longest.took < MAX_TURN,
    "a turn of the writer took {:?} with syncs of {:?}; the bound is {MAX_TURN:?}",
    longest.took,
    PHONE_SYNC_DELAY
  );
}

/// The scenario holds: the read's snapshot kept the purge from truncating the log.
fn assert_log_kept(store: &SlowStore) {
  let length = std::fs::metadata(store.log_path()).unwrap().len();
  assert!(length > 0, "the read held no snapshot: the log was truncated");
}

fn assert_log_truncated(store: &SlowStore) {
  let length = std::fs::metadata(store.log_path()).unwrap().len();
  assert_eq!(length, 0, "the purge left {length} bytes in the log");
}
