//! The user's own writes during a long catch-up on a slow disk (issue #1681). A read thread's
//! action (`ReadQueue::act`: a read marker, a send, a reaction) writes through the engine's
//! connection in a group of its own, so it waits whenever the engine has a group open. The
//! engine's groups end early when somebody waits, between two of their steps: an action must wait
//! for one step of the catch-up at most, not for the backlog, and no group may stay open for long.
//!
//! Measured with 50 ms syncs (900 messages): the engine's steps hold a group for up to about 30 ms;
//! the groups that stay open longer (about 120 ms) are the ones whose commit runs SQLite's
//! automatic checkpoint of the log, two syncs under the writer's lock (left to #1684).
#![allow(clippy::print_stderr, reason = "the waits are printed to compare runs")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::SystemClock;
use teamcraft_core::store::{LocalStore, MessageFilter, StoreError, Table, write_group};
use teamcraft_core::{ClientSettings, PendingEnrollment};
use teamcraft_proto::ConversationId;

use super::{LongGroups, PHONE_SYNC_DELAY, SlowStore};
use crate::common::{Peer, options, settings, settle};

const CHANNELS: usize = 3;
const MESSAGES_PER_CHANNEL: usize = 300;
/// The longest an action may wait for the engine: one step of the catch-up (a message, an MLS
/// entry) and a commit with a checkpoint of the log (two syncs), on a debug build and a busy
/// machine. Waiting for the backlog takes seconds.
const MAX_ACTION_WAIT: Duration = Duration::from_millis(500);
/// The longest the engine's own steps may hold a group open before its commit, for a group the
/// store logged as long: a page of received messages ends after 16 ms when somebody waits.
const MAX_WRITING: Duration = Duration::from_millis(200);

#[tokio::test]
async fn actions_wait_for_one_step_of_a_catch_up() {
  let store = SlowStore::open(PHONE_SYNC_DELAY);
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut carol = PendingEnrollment::new()
    .complete(
      server.add_user("Carol"),
      Arc::clone(&store.writer),
      // A real clock: the writer's turns are measured in milliseconds.
      Box::new(SystemClock),
      ClientSettings {
        catch_up_message_pages: None,
        ..settings()
      },
      options(crate::common::PASSPHRASE),
    )
    .await
    .unwrap();
  let space = alice.space().await;
  let mut channels: Vec<ConversationId> = Vec::new();
  for index in 0..CHANNELS {
    let channel = alice
      .client
      .create_channel(space, &format!("channel {index}"))
      .await
      .unwrap();
    alice.client.add_member(channel, carol.user_id()).await.unwrap();
    channels.push(channel);
  }
  settle(&mut [&mut alice]).await;
  carol.full_sync().await.unwrap();
  for index in 0..MESSAGES_PER_CHANNEL {
    for channel in &channels {
      alice
        .client
        .send_text(*channel, None, &format!("away {index}"))
        .await
        .unwrap();
    }
  }

  let is_done = Arc::new(AtomicBool::new(false));
  let (waits_sender, waits) = mpsc::channel();
  let acting = {
    let lane = store.lane.clone();
    let is_done = Arc::clone(&is_done);
    std::thread::spawn(move || {
      let mut index: u32 = 0;
      while !is_done.load(Ordering::Relaxed) {
        // A read marker, as `mark_read` writes it: a group of its own through the engine's
        // connection.
        let started = Instant::now();
        write_group(&lane, || -> Result<(), StoreError> {
          lane.put(Table::Account, "read marker", &index.to_le_bytes())
        })
        .unwrap();
        waits_sender.send(started.elapsed()).unwrap();
        index += 1;
        std::thread::sleep(Duration::from_millis(5));
      }
    })
  };
  // The engine runs on this thread (a current-thread runtime): its long groups are collected.
  let long_groups = LongGroups::default();
  let collecting = tracing::subscriber::set_default(long_groups.clone());
  let syncs_before = store.disk.tally();
  let started = Instant::now();
  carol.full_sync().await.unwrap();
  let catch_up = started.elapsed();
  drop(collecting);
  let syncs = store.disk.tally().since(syncs_before);
  is_done.store(true, Ordering::Relaxed);
  acting.join().unwrap();

  let waits: Vec<Duration> = waits.try_iter().collect();
  let longest = waits.iter().max().copied().unwrap_or_default();
  let mut sorted = waits.clone();
  sorted.sort();
  let median = sorted.get(sorted.len() / 2).copied().unwrap_or_default();
  eprintln!(
    "catch-up of {} messages on a disk with {PHONE_SYNC_DELAY:?} syncs: {catch_up:?}, {syncs:?}; \
     {} actions, median {median:?}, longest {longest:?}; long groups: {:?}",
    CHANNELS * MESSAGES_PER_CHANNEL,
    waits.len(),
    long_groups.longest_first(),
  );
  for channel in &channels {
    let held = store
      .writer
      .messages(*channel, MessageFilter::TopLevel)
      .unwrap()
      .len();
    assert!(held >= MESSAGES_PER_CHANNEL, "every message arrived: {held}");
  }
  assert!(waits.len() > 1, "the actions ran during the catch-up: {waits:?}");
  assert!(
    longest < MAX_ACTION_WAIT,
    "an action waited {longest:?} during a catch-up of {catch_up:?}"
  );
  let max_writing_ms = u64::try_from(MAX_WRITING.as_millis()).unwrap();
  for group in long_groups.longest_first() {
    assert!(
      group.writing_ms < max_writing_ms,
      "a group held the writer for {} ms before its commit: {group:?}",
      group.writing_ms
    );
  }
}
