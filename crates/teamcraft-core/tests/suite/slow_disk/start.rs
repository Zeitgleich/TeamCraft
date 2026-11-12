//! The signed-in start on a phone (issue #1758): the engine loads the MLS state while the main read
//! thread builds the chat list, on a disk whose reads come from flash. Android 1.0.30 took 4.7 s for
//! the one and 2.6 s for the other with 83 conversations, and everything queued behind them waited.
//! The client now opens the MLS state on demand (a group's entries when it is used), and the app's
//! first chat list is the one the last pass kept (`teamcraft-ffi`, `reads.rs`): one record.
#![cfg(feature = "ui")]
#![allow(clippy::print_stderr, reason = "the numbers are what the measurement is for")]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::SystemClock;
use teamcraft_core::mls_storage;
use teamcraft_core::records;
use teamcraft_core::store::{LaneStore, LocalStore as _, SealedSqliteStore, Table};
use teamcraft_core::ui_backend::{CoreChatBackend, EntryNeed};
use teamcraft_core::{Client, ClientSettings, PendingEnrollment};
use teamcraft_proto::{ConversationId, SpaceId, UserId};

use super::{DiskTally, PHONE_SYNC_DELAY, SlowDisk, SlowStore};
use crate::common::{Peer, options, settings, settle};

/// What the start measured in 1.0.30 had.
const CHANNELS: usize = 83;
const MESSAGES_PER_CHANNEL: usize = 20;
/// A read that misses every cache on a phone's flash (a few hundred microseconds on a mid-range
/// phone at start, while other threads read too).
const PHONE_READ_DELAY: Duration = Duration::from_micros(300);
/// What opening the MLS state may read, whatever the number of groups: the old record's marker and
/// the generation, a few pages each.
const MAX_OPEN_READS: u64 = 16;
/// Where the measurement keeps a list like the app's kept chat list.
const KEPT_LIST_KEY: &str = "measured_chat_list";

/// Opening the MLS state reads no group's entries: a start costs the same with 3 groups or 300.
#[tokio::test]
async fn opening_the_mls_state_reads_no_group() {
  let (store, _) = seeded(6, 1, 0).await;
  let store = store.reopen();
  let (opened, reads) = measure(&store.disk, || {
    mls_storage::open_mls_storage(&Arc::clone(&store.writer)).unwrap()
  });
  assert!(
    reads.disk.reads <= MAX_OPEN_READS,
    "opening read {} pages",
    reads.disk.reads
  );
  let (loaded, whole) = measure(&store.disk, || {
    mls_storage::load_mls_storage(store.writer.as_ref()).unwrap()
  });
  assert!(whole.disk.reads > reads.disk.reads * 4, "{whole} against {reads}");
  let held = |provider: &openmls_rust_crypto::OpenMlsRustCrypto| {
    use openmls_traits::OpenMlsProvider as _;
    provider.storage().values.read().unwrap().len()
  };
  assert_eq!(held(&opened.0), 0);
  assert!(held(&loaded.0) >= 6 * 10, "a dozen entries per group");
}

/// The numbers of docs/performance.md:
/// `cargo test --release -p teamcraft-core --all-features --test suite -- --ignored --nocapture measure_signed_in_start`
/// (`START_CHANNELS`, `START_MESSAGES` and `START_MEMBERS` change the workspace).
#[tokio::test]
#[ignore = "measurement; prints what the start reads and how long it takes"]
async fn measure_signed_in_start() {
  let channels = env_or("START_CHANNELS", CHANNELS);
  let messages = env_or("START_MESSAGES", MESSAGES_PER_CHANNEL);
  let members = env_or("START_MEMBERS", 3);
  let started = Instant::now();
  let (store, me) = seeded(channels, messages, members).await;
  eprintln!(
    "seeded {channels} channels of {messages} messages, {members} more members each, in {:?}",
    started.elapsed()
  );
  let store = store.reopen();
  store.disk.set_delays(PHONE_SYNC_DELAY, PHONE_READ_DELAY);

  let (_, whole) = measure(&store.disk, || {
    mls_storage::load_mls_storage(store.writer.as_ref()).unwrap();
  });
  eprintln!("MLS state, loaded whole:   {whole}");
  let backend = list_backend(&store.lane, me);
  let (listed, list) = measure(&store.disk, || list_pass(&backend));
  eprintln!(
    "chat list, a pass:         {list} ({} conversations)",
    listed.len()
  );
  let (_, again) = measure(&store.disk, || list_pass(&backend).len());
  eprintln!("chat list, a pass again:   {again}");
  // Like the app's kept list (the FFI's items are about the size of these).
  records::put_json(store.writer.as_ref(), Table::Account, KEPT_LIST_KEY, &listed).unwrap();

  let store = store.reopen();
  store.disk.set_delays(PHONE_SYNC_DELAY, PHONE_READ_DELAY);
  let (_opened, opened) = measure(&store.disk, || {
    mls_storage::open_mls_storage(&Arc::clone(&store.writer)).unwrap()
  });
  eprintln!("MLS state, on demand:      {opened}");
  let (kept, read_kept) = measure(&store.disk, || {
    store
      .lane
      .get(Table::Account, KEPT_LIST_KEY)
      .unwrap()
      .unwrap()
      .len()
  });
  eprintln!("chat list, the kept one:   {read_kept} ({} KiB)", kept / 1024);
}

/// A store as a phone holds it after a while: Carol is in `channels` channels of Alice's with
/// `members` more people, each with `messages` messages she caught up on, and her chat list was read
/// once (its counts are kept).
async fn seeded(channels: usize, messages: usize, members: usize) -> (SlowStore, UserId) {
  let store = SlowStore::open(Duration::ZERO);
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut others = Vec::with_capacity(members);
  for index in 0..members {
    others.push(Peer::enroll(&server, &format!("Member {index}")).await);
  }
  let mut carol = enroll_on(&server, &store.writer).await;
  let me = carol.user_id();
  let space = alice.space().await;
  let mut created: Vec<ConversationId> = Vec::with_capacity(channels);
  for index in 0..channels {
    let channel = alice
      .client
      .create_channel(space, &format!("channel {index}"))
      .await
      .unwrap();
    alice.client.add_member(channel, me).await.unwrap();
    for other in &others {
      alice
        .client
        .add_member(channel, other.client.user_id())
        .await
        .unwrap();
    }
    created.push(channel);
  }
  settle(&mut [&mut alice]).await;
  for index in 0..messages {
    for channel in &created {
      alice
        .client
        .send_text(
          *channel,
          None,
          &format!("A message of the usual length, number {index}."),
        )
        .await
        .unwrap();
    }
  }
  carol.full_sync().await.unwrap();
  drop(carol);
  let backend = list_backend(&store.lane, me);
  assert!(list_pass(&backend).len() >= channels, "Carol lists every channel");
  (store, me)
}

async fn enroll_on(
  server: &FakeServer,
  store: &Arc<SealedSqliteStore>,
) -> Client<teamcraft_core::api::fake::FakeServerApi, Arc<SealedSqliteStore>> {
  PendingEnrollment::new()
    .complete(
      server.add_user("Carol"),
      Arc::clone(store),
      Box::new(SystemClock),
      ClientSettings {
        catch_up_message_pages: None,
        ..settings()
      },
      options(crate::common::PASSPHRASE),
    )
    .await
    .unwrap()
}

/// The main read thread's backend, as the apps set it up.
fn list_backend(lane: &LaneStore, me: UserId) -> CoreChatBackend<LaneStore> {
  CoreChatBackend::new(lane.clone(), me, Box::new(SystemClock)).with_entry_need(EntryNeed::Unread)
}

/// The apps' chat list read (`conversations` of the FFI): the spaces, then one pass.
fn list_pass(backend: &CoreChatBackend<LaneStore>) -> Vec<teamcraft_client_api::model::Conversation> {
  let spaces: BTreeSet<SpaceId> = backend.spaces().into_iter().map(|space| space.id).collect();
  backend.listed_conversations(&spaces)
}

/// What a step took and read.
struct Measured {
  took: Duration,
  disk: DiskTally,
}

impl std::fmt::Display for Measured {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "{:>7.1} ms, {:>6} reads, {} syncs",
      ms(self.took),
      self.disk.reads,
      self.disk.syncs()
    )
  }
}

fn measure<T>(disk: &SlowDisk, step: impl FnOnce() -> T) -> (T, Measured) {
  let before = disk.tally();
  let started = Instant::now();
  let result = step();
  let measured = Measured {
    took: started.elapsed(),
    disk: disk.tally().since(before),
  };
  (result, measured)
}

fn env_or(name: &str, default: usize) -> usize {
  std::env::var(name)
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(default)
}

fn ms(duration: Duration) -> f64 {
  duration.as_secs_f64() * 1000.0
}
