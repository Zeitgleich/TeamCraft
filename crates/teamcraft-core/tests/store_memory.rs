//! The sealed store's budgets for a large workspace (issue #155, docs/performance.md, "Local
//! store"): opening costs nothing that grows with the history, a page of a conversation costs that
//! page, and the iOS notification extension shows a message within a few MiB of its ~24 MB.
//! Its own test binary: the counting allocator sees every allocation of the process, so no other
//! test may run beside it.
//!
//! `TEAMCRAFT_STORE_BUDGET_MESSAGES` sets the workspace's size (default 20,000 messages in 300
//! conversations, half of them in one channel; docs/performance.md has the numbers for 100,000,
//! measured with `--release`).
#![cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
#![allow(clippy::unwrap_used)]

#[path = "common/mod.rs"]
mod common;

use std::time::{Duration, Instant};

use common::{Peer, settle};
use peak_alloc::PeakAlloc;
use rand_core::OsRng;
use teamcraft_core::api::fake::{FakeServer, FakeServerApi};
use teamcraft_core::push_preview::{PreviewContent, push_preview};
use teamcraft_core::store::{
  LocalStore, MessageContent, MessageEdge, MessageFilter, MessageSlot, SealedSqliteStore, SealedStoreLimits,
  StaticKeySource, StoredMessage, Table, write_group,
};
use teamcraft_crypto::push::seal_wakeup;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{
  ClientMessageId, ConversationId, Id, MessageId, PushKind, PushProvider, PushWakeUp, Snowflake, UserId,
};

#[global_allocator]
static ALLOCATOR: PeakAlloc = PeakAlloc;

const MIB: usize = 1024 * 1024;
const CONVERSATIONS: i64 = 300;
const DEFAULT_MESSAGES: usize = 20_000;
/// The extension's share for opening the store and finding the message; measured about 1 MiB.
const MAX_EXTENSION_BYTES: usize = 8 * MIB;
/// Generous for a debug build on a busy CI runner; measured in milliseconds.
const MAX_OPEN: Duration = Duration::from_secs(1);
const MAX_PAGE: Duration = Duration::from_millis(200);
const NOTIFIED_BODY: &str = "the message the notification shows";
const HISTORY_BODY: &str =
  "an ordinary message of the history, about a hundred bytes long, as most of them are";

#[tokio::test]
async fn a_large_store_opens_and_shows_a_notification_within_budget() {
  let messages: usize = std::env::var("TEAMCRAFT_STORE_BUDGET_MESSAGES")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(DEFAULT_MESSAGES);
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let directory =
    std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("store-memory-{}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let path = directory.join("store.db");
  let key = SymmetricKey::generate(&mut OsRng);

  let filled = Instant::now();
  let notified = {
    let store = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
    copy_account(&bob, &store);
    fill_history(&store, channel, alice.client.user_id(), messages)
  };
  let fill_time = filled.elapsed();
  let sealed = wake_up_for(&server, &bob, alice.client.user_id(), channel, notified);

  // The notification extension: a cold open with its limits, then the preview from the store.
  let before = ALLOCATOR.current_usage();
  ALLOCATOR.reset_peak_usage();
  let started = Instant::now();
  let store = SealedSqliteStore::open_with_limits(
    &path,
    &StaticKeySource::new(key.duplicate()),
    SealedStoreLimits::minimal(),
  )
  .unwrap();
  let preview = push_preview::<FakeServerApi>(&store, None, &sealed)
    .await
    .unwrap();
  let extension_time = started.elapsed();
  let extension_bytes = ALLOCATOR.peak_usage().saturating_sub(before);
  drop(store);
  assert_eq!(preview.content, PreviewContent::Text(NOTIFIED_BODY.into()));

  // The app: open, then the newest page of a conversation and one message far back.
  let started = Instant::now();
  let store = SealedSqliteStore::open(&path, &StaticKeySource::new(key)).unwrap();
  let open_time = started.elapsed();
  let started = Instant::now();
  let newest = store
    .edge_server_id(channel, MessageEdge::Newest)
    .unwrap()
    .unwrap();
  let page = store.messages_at(channel, MessageSlot::At(newest)).unwrap();
  let oldest = store
    .edge_server_id(channel, MessageEdge::Oldest)
    .unwrap()
    .unwrap();
  assert!(store.message_by_server_id(channel, oldest).unwrap().is_some());
  let page_time = started.elapsed();
  assert_eq!(page.len(), 1);
  let before = ALLOCATOR.current_usage();
  ALLOCATOR.reset_peak_usage();
  let started = Instant::now();
  let whole = store.messages(channel, MessageFilter::All).unwrap().len();
  let whole_time = started.elapsed();
  let whole_bytes = ALLOCATOR.peak_usage().saturating_sub(before);

  println!(
    "{messages} messages in {CONVERSATIONS} conversations (filled in {fill_time:?}): extension open + \
     preview {extension_time:?} with {:.2} MiB at its peak; app open {open_time:?}; newest and oldest \
     message {page_time:?}; all {whole} of the channel {whole_time:?} with {:.2} MiB at its peak",
    extension_bytes as f64 / MIB as f64,
    whole_bytes as f64 / MIB as f64
  );
  assert!(
    extension_bytes < MAX_EXTENSION_BYTES,
    "the extension took {} MiB",
    extension_bytes / MIB
  );
  assert!(open_time < MAX_OPEN, "opening took {open_time:?}");
  assert!(page_time < MAX_PAGE, "two messages took {page_time:?}");
  drop(store);
  std::fs::remove_dir_all(&directory).unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
}

async fn channel_with_two() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "planning")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  bob
    .client
    .register_push(PushProvider::WebPush, "{}")
    .await
    .unwrap();
  (server, alice, bob, channel)
}

/// Bob's account as his app keeps it: every entry (the push key, names, the policy).
fn copy_account(bob: &Peer, store: &SealedSqliteStore) {
  write_group(store, || {
    for table in Table::ALL {
      for (key, value) in bob.store.list(table)? {
        store.put(table, &key, &value)?;
      }
    }
    Ok::<_, teamcraft_core::store::StoreError>(())
  })
  .unwrap();
}

/// `messages`, half of them in the channel and the rest spread over the other conversations; the
/// channel's newest is the one notified, whose server ID this returns.
fn fill_history(
  store: &SealedSqliteStore,
  channel: ConversationId,
  sender: UserId,
  messages: usize,
) -> MessageId {
  let messages = i64::try_from(messages).unwrap();
  let in_channel = messages / 2;
  let per_other = (messages - in_channel) / (CONVERSATIONS - 1);
  let conversations =
    std::iter::once((channel, in_channel)).chain((1..CONVERSATIONS).map(|n| (id(1_000_000 + n), per_other)));
  for (index, (conversation, count)) in conversations.enumerate() {
    let index = i64::try_from(index).unwrap();
    for start in (0..count).step_by(1_000) {
      write_group(store, || {
        for n in start..(start + 1_000).min(count) {
          store.upsert_message(&history(conversation, sender, index, n, HISTORY_BODY))?;
        }
        Ok::<_, teamcraft_core::store::StoreError>(())
      })
      .unwrap();
    }
  }
  let notified = history(channel, sender, 0, in_channel, NOTIFIED_BODY);
  store.upsert_message(&notified).unwrap();
  notified.maybe_server_id.unwrap()
}

fn history(conversation: ConversationId, sender: UserId, index: i64, n: i64, body: &str) -> StoredMessage {
  let mut client_message_id = [0u8; 16];
  client_message_id[..8].copy_from_slice(&index.to_be_bytes());
  client_message_id[8..].copy_from_slice(&n.to_be_bytes());
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

/// Seals what the server would push for the message.
fn wake_up_for(
  server: &FakeServer,
  bob: &Peer,
  sender: UserId,
  channel: ConversationId,
  message: MessageId,
) -> Vec<u8> {
  let registration = server.push_registration(bob.client.device_id()).unwrap();
  let key = SymmetricKey::from_slice(registration.push_key.as_slice()).unwrap();
  let wake_up = PushWakeUp {
    kind: PushKind::Message,
    conversation_id: channel,
    maybe_message_id: Some(message),
    maybe_sender_id: Some(sender),
    is_urgent: false,
    maybe_thread_id: None,
    maybe_call_id: None,
    maybe_badge: None,
    maybe_sent_at: None,
  };
  seal_wakeup(&key, &serde_json::to_vec(&wake_up).unwrap(), &mut OsRng).unwrap()
}

fn id<T: Id>(raw: i64) -> T {
  T::from_snowflake(Snowflake::new(raw).unwrap())
}
