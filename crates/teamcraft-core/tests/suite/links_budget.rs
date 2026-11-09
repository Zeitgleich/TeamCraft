//! What the Files & links tab's links cost on a large workspace (issue #609, robustness.md,
//! "Performance and resources"). A measurement, not a check: run it by hand, with `--release`:
//!
//! `cargo test --release -p teamcraft-core --all-features --test suite -- --ignored --nocapture
//! measure_the_links_tab`
//!
//! 100,000 messages in 300 conversations of a sealed SQLite store (the desktop's), half of them in
//! one. In that one every 1,000th message has a link: few links in a long history, the case the
//! issue is about. In the others every 10th has one. "Before" is the scan the tab did
//! (`links::links_page`: messages newest first until the page is full); "after" is the core's read
//! of its link index (`conversation_links`). The after runs go first, so the peak memory each adds
//! is its own.
#![cfg(all(feature = "ui", feature = "sealed-sqlite"))]
#![allow(clippy::print_stderr, reason = "the numbers are what this test is for")]
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::links::{LINKS_PER_PAGE, LinkPage, links_page};
use teamcraft_core::clock::SystemClock;
use teamcraft_core::records;
use teamcraft_core::store::{
  LocalStore, MessageContent, SealedSqliteStore, StaticKeySource, StoredMessage, Table, write_group,
};
use teamcraft_core::ui_backend::CoreChatBackend;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{
  ChannelModeration, ClientMessageId, ConversationId, ConversationResponse, Id, Snowflake, UserId,
};

const CONVERSATIONS: i64 = 300;
const MESSAGES: i64 = 100_000;
/// In the large conversation: every 1,000th message has a link.
const SPARSE_LINK_EVERY: i64 = 1_000;
/// In the others: every 10th.
const DENSE_LINK_EVERY: i64 = 10;
const RUNS: usize = 10;

#[test]
#[ignore = "a measurement: run by hand with --release"]
fn measure_the_links_tab() {
  let directory = std::env::temp_dir().join(format!("teamcraft-links-budget-{}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let store = Arc::new(
    SealedSqliteStore::open(
      &directory.join("store.db"),
      &StaticKeySource::new(SymmetricKey::from_bytes([7; 32])),
    )
    .unwrap(),
  );
  let me: UserId = id(76);
  let started = Instant::now();
  seed(store.as_ref(), me);
  eprintln!("seeded {MESSAGES} messages in {:?}", started.elapsed());
  eprintln!("peak memory after seeding: {} MiB", peak_mib());
  let backend = CoreChatBackend::new(store.clone(), me, Box::new(SystemClock));

  let cases = [
    ("large, sparse links, 1 page ", conversation_id(0), LINKS_PER_PAGE),
    (
      "large, sparse links, 2 pages",
      conversation_id(0),
      2 * LINKS_PER_PAGE,
    ),
    ("small, dense links, 1 page  ", conversation_id(1), LINKS_PER_PAGE),
  ];
  for (label, conversation, limit) in cases {
    let after = measure(|| backend.conversation_links(conversation, limit).unwrap());
    eprintln!(
      "after:  {label}  p50 {:>8.2} ms  ({} links)  peak {} MiB",
      ms(after.0),
      after.1,
      peak_mib()
    );
  }
  for (label, conversation, limit) in cases {
    let before = measure(|| links_page(&backend, conversation, limit));
    eprintln!(
      "before: {label}  p50 {:>8.2} ms  ({} links)  peak {} MiB",
      ms(before.0),
      before.1,
      peak_mib()
    );
  }
  drop(backend);
  drop(store);
  std::fs::remove_dir_all(&directory).unwrap();
}

/// The median of [`RUNS`] reads and how many links a read found.
fn measure(read: impl Fn() -> LinkPage) -> (Duration, usize) {
  let mut runs: Vec<Duration> = Vec::with_capacity(RUNS);
  let mut found = 0;
  for _ in 0..RUNS {
    let started = Instant::now();
    found = read().links.len();
    runs.push(started.elapsed());
  }
  runs.sort();
  assert!(found > 0, "a measurement of nothing would look very good");
  (runs[RUNS / 2], found)
}

fn seed(store: &SealedSqliteStore, me: UserId) {
  let peer: UserId = id(77);
  let in_first = MESSAGES / 2;
  let per_other = (MESSAGES - in_first) / (CONVERSATIONS - 1);
  for index in 0..CONVERSATIONS {
    let conversation = conversation_id(index);
    records::put_json(
      store,
      Table::Conversations,
      &conversation.to_string(),
      &conversation_record(conversation, me, peer),
    )
    .unwrap();
    let (count, link_every) = if index == 0 {
      (in_first, SPARSE_LINK_EVERY)
    } else {
      (per_other, DENSE_LINK_EVERY)
    };
    for start in (0..count).step_by(1_000) {
      write_group(store, || {
        for n in start..(start + 1_000).min(count) {
          store.upsert_message(&message(conversation, peer, index, n, n % link_every == 0))?;
        }
        Ok::<_, teamcraft_core::store::StoreError>(())
      })
      .unwrap();
    }
  }
}

fn conversation_record(conversation: ConversationId, me: UserId, peer: UserId) -> serde_json::Value {
  let person = |user: UserId, name: &str| serde_json::json!({ "id": user, "display_name": name, "kind": "member", "is_deactivated": false });
  let record = serde_json::json!({
    "id": conversation,
    "kind": "dm",
    "visibility": "private",
    "history_policy": "from_join",
    "is_archived": false,
    "moderation": ChannelModeration::default(),
    "contains_guests": false,
    "participants": [person(me, "Alice"), person(peer, "Sam Doe")],
  });
  // The store takes any JSON, but a record the app can't decode is skipped when read.
  serde_json::from_value::<ConversationResponse>(record.clone()).unwrap();
  record
}

fn message(
  conversation: ConversationId,
  sender: UserId,
  index: i64,
  n: i64,
  has_link: bool,
) -> StoredMessage {
  let mut client_message_id = [0u8; 16];
  client_message_id[..8].copy_from_slice(&index.to_be_bytes());
  client_message_id[8..].copy_from_slice(&n.to_be_bytes());
  let body = if has_link {
    format!("The notes are at https://example.com/notes/{n} for everyone.")
  } else {
    "A message of the usual length in a long history.".into()
  };
  StoredMessage {
    conversation_id: conversation,
    client_message_id: ClientMessageId::from_bytes(client_message_id),
    maybe_server_id: Some(id(1_000_000_000 + index * 10_000_000 + n)),
    sender_user_id: sender,
    maybe_thread_id: None,
    sent_at_ms: 1_700_000_000_000 + n,
    content: MessageContent::text(body, Vec::new()),
    is_edited: false,
    edits: Vec::new(),
    maybe_edit_id: None,
    is_pinned: false,
    maybe_verified_commit: None,
    maybe_server_change: None,
    maybe_unreadable: None,
  }
}

/// The process's peak resident memory (Linux; 0 elsewhere).
fn peak_mib() -> u64 {
  std::fs::read_to_string("/proc/self/status")
    .ok()
    .and_then(|status| {
      status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
    })
    .map_or(0, |kib| kib / 1024)
}

fn conversation_id(index: i64) -> ConversationId {
  id(1_000_000 + index)
}

fn id<T: Id>(raw: i64) -> T {
  T::from_snowflake(Snowflake::new(raw).unwrap())
}

fn ms(duration: Duration) -> f64 {
  duration.as_secs_f64() * 1000.0
}
