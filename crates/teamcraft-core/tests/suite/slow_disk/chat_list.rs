//! The chat list pass after a start or a reconnect (issue #1819): on phones it held the main read
//! thread for 1.7–2.7 s in 1.0.32, waiting on page reads while the catch-up wrote. A pass now reads
//! the few conversations with a catch-up mark or lines read from the store's key directory once per
//! slice, instead of looking each one up per conversation (most found nothing, after several page
//! reads each), skips what the apps' list doesn't show (a channel's members, bookmarks), and runs
//! in slices the read thread hands back between.
#![cfg(feature = "ui")]

use teamcraft_core::chat_list_pass::SliceLimit;

use super::start::{list_backend, list_pass, measure, seeded};

/// Two workspaces whose difference is what one more conversation costs a pass.
const FEWER: usize = 6;
const MORE: usize = 38;
/// Page reads a cold pass may take per conversation more. On one machine the pass read 17 to 19
/// before issue #1819 and 11 to 14 after it: the conversation's record in the list (about 4) and
/// what checks its kept tally (feed version, length, the tally's last row). How SQLite lays out the
/// file and the machine move it by several pages (CI's runner read 17 after the change), so this
/// bound can't tell the two apart: it catches what grows much faster, such as an entry reading its
/// messages or a table whole.
const MAX_READS_PER_CONVERSATION: u64 = 24;

/// What one more conversation costs a pass on a store just opened: a few pages, not a lookup per
/// record the entry might have.
#[tokio::test]
async fn a_cold_pass_reads_a_few_pages_per_conversation() {
  let fewer = cold_pass_reads(FEWER).await;
  let more = cold_pass_reads(MORE).await;
  let per_conversation = more.saturating_sub(fewer) / u64::try_from(MORE - FEWER).unwrap();
  assert!(
    per_conversation <= MAX_READS_PER_CONVERSATION,
    "{per_conversation} page reads per conversation ({fewer} for {FEWER}, {more} for {MORE})"
  );
}

/// A pass of one entry per slice lists what one pass lists, in the same order.
#[tokio::test]
async fn a_pass_in_slices_lists_what_a_whole_pass_lists() {
  let (store, me) = seeded(4, 2, 1).await;
  let backend = list_backend(&store.lane, me);
  let whole = list_pass(&backend);
  let mut pass = backend.begin_list_pass(SliceLimit {
    max_ms: i64::MAX,
    max_entries: 1,
  });
  let mut sliced = Vec::new();
  while !pass.is_done() {
    backend.continue_list_pass(&mut pass);
    sliced.extend(pass.take_listed());
    assert_eq!(sliced.len(), pass.slices(), "one entry per slice");
  }
  assert_eq!(sliced, whole);
}

/// The page reads of a pass right after opening a store with `channels` channels.
async fn cold_pass_reads(channels: usize) -> u64 {
  let (store, me) = seeded(channels, 2, 1).await;
  let store = store.reopen();
  let backend = list_backend(&store.lane, me);
  let (listed, pass) = measure(&store.disk, || list_pass(&backend));
  assert!(listed.len() >= channels, "every channel is listed");
  pass.disk.reads
}
