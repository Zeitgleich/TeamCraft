//! The purge of the write-ahead log after a deleted key (docs/e2ee.md §4.3.1) while a read thread
//! holds a snapshot. It can't truncate the log then, and 1.0.29 tried again at the end of every
//! group of writes, each time waiting the connection's busy timeout (a second) under the writer's
//! lock: every write and every read that needed the writer queued behind it (#1657, #1678).
#![allow(clippy::print_stderr, reason = "the turns are printed to compare runs")]

use std::time::Duration;

use teamcraft_core::store::{LocalStore, SealedSqliteStore, StoreError, Table, write_group};

use super::{PHONE_SYNC_DELAY, SlowStore, Turn, measure};

/// Groups of writes while the read holds its snapshot.
const GROUPS: u8 = 8;
/// The longest a turn of the writer may take, ten times [`PHONE_SYNC_DELAY`]: a group with one try of the purge (its syncs) and a
/// debug build on a busy machine. 1.0.29 waited the busy timeout, a second, in each.
const MAX_TURN: Duration = Duration::from_millis(500);
/// The syncs all [`GROUPS`] may cost together: none for the groups themselves (the log is synced
/// at checkpoints only), and one more try of the purge if the machine stalls past its retry
/// interval. A try after every group (main before #1678) syncs the log in each.
const MAX_GROUP_SYNCS: u64 = 2;

#[test]
fn a_purge_that_a_read_holds_up_does_not_hold_up_the_writer() {
  let store = SlowStore::open(PHONE_SYNC_DELAY);
  seed(&store);
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
  // Once the read is over the purge finishes: the deleted key's pages leave the log.
  let finish = measure(&store.disk, || store.writer.purge_freed());

  eprintln!("turns: {turns:?}; finishing the purge: {finish:?}");
  assert_turns_bounded(&turns);
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
    // The lane reads again and sees the newest commit, as reads do all the time: each try of the
    // purge then finds new frames to write back and syncs, and still can't truncate.
    store.second_reader.end_reads();
    pin(&store.second_reader);
    turns.push(measure(&store.disk, || put_in_group(&store.writer, index)));
  }
  store.second_reader.end_reads();

  let group_syncs: u64 = turns.iter().map(|turn| turn.syncs.syncs()).sum();
  eprintln!("deletion: {deletion:?}; groups: {turns:?}; {group_syncs} syncs");
  assert_turns_bounded(&turns);
  assert!(
    group_syncs <= MAX_GROUP_SYNCS,
    "{GROUPS} groups synced {group_syncs} times: the purge was tried after every group"
  );
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
