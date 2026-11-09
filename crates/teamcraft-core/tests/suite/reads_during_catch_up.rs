//! Issue #1575: on Android and the desktop the UI's reads run on a thread of their own, against the
//! same sealed SQLite store the engine thread writes a catch-up into. A long backlog must not hold
//! them up for its whole length: each read waits for one turn of the writer at most.
#![cfg(feature = "sealed-sqlite")]
#![allow(clippy::print_stderr, reason = "the waits are printed to compare runs")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use crate::common::{Peer, options, settings, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::SystemClock;
use teamcraft_core::store::{
  LocalStore, MessageFilter, SealedSqliteStore, StaticKeySource, StoreError, read_together,
};
use teamcraft_core::{ClientSettings, PendingEnrollment};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::ConversationId;

const CHANNELS: usize = 3;
const MESSAGES_PER_CHANNEL: usize = 150;
/// A read waits for one group of the writer, not for the backlog: the writer's groups end after
/// 16 ms when a read waits, plus one message and the commit, on a debug build and a busy machine.
const MAX_READ_WAIT: Duration = Duration::from_millis(500);

#[tokio::test]
async fn reads_take_turns_with_a_long_catch_up() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let directory =
    std::env::temp_dir().join(format!("teamcraft-reads-during-catch-up-{}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let key = StaticKeySource::new(SymmetricKey::generate(&mut rand_core::OsRng));
  let store = Arc::new(SealedSqliteStore::open(&directory.join("store.db"), &key).unwrap());
  let mut carol = PendingEnrollment::new()
    .complete(
      server.add_user("Carol"),
      store.clone(),
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
  let reader = {
    let store = store.clone();
    let channels = channels.clone();
    let is_done = is_done.clone();
    std::thread::spawn(move || {
      while !is_done.load(Ordering::Relaxed) {
        let started = Instant::now();
        let held = read_together(&*store, || -> Result<usize, StoreError> {
          let mut held = 0;
          for channel in &channels {
            held += store.messages(*channel, MessageFilter::TopLevel)?.len();
          }
          Ok(held)
        })
        .unwrap();
        waits_sender.send((started.elapsed(), held)).unwrap();
        std::thread::sleep(Duration::from_millis(2));
      }
    })
  };
  let started = Instant::now();
  carol.full_sync().await.unwrap();
  let catch_up = started.elapsed();
  is_done.store(true, Ordering::Relaxed);
  reader.join().unwrap();
  let _ = std::fs::remove_dir_all(&directory);

  let waits: Vec<(Duration, usize)> = waits.try_iter().collect();
  let longest = waits.iter().map(|(wait, _)| *wait).max().unwrap_or_default();
  let mut seen: Vec<usize> = waits.iter().map(|(_, held)| *held).collect();
  seen.dedup();
  eprintln!(
    "catch-up of {} messages: {catch_up:?}; {} reads, the longest {longest:?}, {} states seen",
    CHANNELS * MESSAGES_PER_CHANNEL,
    waits.len(),
    seen.len()
  );
  for channel in &channels {
    let held = store.messages(*channel, MessageFilter::TopLevel).unwrap().len();
    // The channel's own line about Carol joining comes with them.
    assert!(held >= MESSAGES_PER_CHANNEL, "every message arrived: {held}");
  }
  assert!(
    longest < MAX_READ_WAIT,
    "a read waited {longest:?} during a catch-up of {catch_up:?}"
  );
  // The reads saw the backlog arrive, not only its start and its end.
  assert!(seen.len() > CHANNELS * 2, "states seen: {seen:?}");
}
