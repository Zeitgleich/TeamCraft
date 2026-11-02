//! What opening the app and switching channels cost on a large workspace, through the API the apps
//! use (robustness.md, "Performance and resources"; docs/performance.md, "Mobile"). A measurement,
//! not a check: run it by hand, with `--release`, on a real store's size:
//!
//! `cargo test --release -p teamcraft-ffi --test start_budget -- --ignored --nocapture`
//!
//! `TEAMCRAFT_STORE_BUDGET_MESSAGES` sets the messages (default 100,000 in 300 conversations, half
//! of them in one). MLS state is filler of a plausible size, the sync loop finds no server.

#![allow(clippy::print_stderr, reason = "the numbers are what this test is for")]
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::SystemClock;
use teamcraft_core::records;
use teamcraft_core::store::{
  LocalStore, MessageContent, SealedSqliteStore, StaticKeySource, StoredMessage, Table, write_group,
};
use teamcraft_core::{ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_ffi::{AccountState, AppConfig, EventListener, TeamCraft, TokenProvider};
use teamcraft_proto::{
  ChannelModeration, ClientMessageId, ConversationId, ConversationResponse, Id, Snowflake, UserId,
};

const KEY: [u8; 32] = [7; 32];
const ACCOUNT: &str = "account";
const CONVERSATIONS: i64 = 300;
const DEFAULT_MESSAGES: i64 = 100_000;
/// About ten stored MLS values per group.
const MLS_ENTRIES: usize = 3_000;
const MLS_ENTRY_BYTES: usize = 4 * 1024;
/// A token refresh over a slow link: what the app's sign-in library blocks for.
const SLOW_REFRESH: Duration = Duration::from_secs(3);
const PAGE: u32 = 50;
const SWITCHES: usize = 100;
const UNREACHABLE_SERVER: &str = "http://127.0.0.1:9";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement: run by hand with --release"]
async fn measure_start_and_channel_switch() {
  let directory = std::env::temp_dir().join(format!("teamcraft-start-budget-{}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let messages = std::env::var("TEAMCRAFT_STORE_BUDGET_MESSAGES")
    .ok()
    .and_then(|text| text.parse().ok())
    .unwrap_or(DEFAULT_MESSAGES);
  let started = Instant::now();
  seed(&directory, messages).await;
  eprintln!("seeded {messages} messages in {:?}", started.elapsed());

  eprintln!("-- signed in, token valid");
  measure(&directory, Arc::new(ValidToken)).await;
  eprintln!("-- signed in, the access token ran out: the refresh takes {SLOW_REFRESH:?}");
  measure(&directory, Arc::new(SlowRefresh(Mutex::new(())))).await;

  std::fs::remove_dir_all(&directory).unwrap();
}

async fn measure(directory: &std::path::Path, tokens: Arc<dyn TokenProvider>) {
  let opened = Instant::now();
  let core = TeamCraft::new(config(directory), tokens, Arc::new(Quiet)).unwrap();
  eprintln!("constructor                 {:>8.1} ms", ms(opened.elapsed()));
  let state = core.account_state().await.unwrap();
  assert!(matches!(state, AccountState::Ready { .. }));
  eprintln!("account state               {:>8.1} ms", ms(opened.elapsed()));
  core.start().await.unwrap();
  eprintln!("start of the sync           {:>8.1} ms", ms(opened.elapsed()));
  let snapshot = core.snapshot().await.unwrap();
  // A measurement of nothing would look very good.
  assert!(
    core.conversations().await.unwrap().len() >= CONVERSATIONS as usize,
    "the seeded conversations are not what the app reads"
  );
  eprintln!(
    "first snapshot ({:>4} KiB)   {:>8.1} ms",
    snapshot.len() / 1024,
    ms(opened.elapsed())
  );
  eprintln!(
    "signed-in start: list usable {:>7.1} ms (budget 2000)",
    ms(opened.elapsed())
  );

  let again = Instant::now();
  core.snapshot().await.unwrap();
  eprintln!("snapshot, nothing changed   {:>8.1} ms", ms(again.elapsed()));

  let mut switches = Vec::with_capacity(SWITCHES);
  for n in 0..SWITCHES {
    let conversation = conversation_id(i64::try_from(n).unwrap() % CONVERSATIONS);
    let switch = Instant::now();
    let tail = core
      .message_tail(conversation.to_string(), None, 0, PAGE)
      .await
      .unwrap();
    switches.push(switch.elapsed());
    let tail: serde_json::Value = serde_json::from_str(&tail).unwrap();
    let shown = tail["messages"].as_array().map_or(0, Vec::len);
    assert!(shown > 0, "no messages in a seeded conversation");
  }
  let revisit = Instant::now();
  core
    .message_tail(conversation_id(0).to_string(), None, 0, PAGE)
    .await
    .unwrap();
  let revisit = revisit.elapsed();
  switches.sort();
  eprintln!(
    "channel switch: p50 {:.1} ms, p95 {:.1} ms (budget 150), the big channel again {:.1} ms",
    ms(switches[SWITCHES / 2]),
    ms(switches[SWITCHES * 95 / 100]),
    ms(revisit)
  );
  core.stop().await.unwrap();
}

async fn seed(directory: &std::path::Path, messages: i64) {
  let store = SealedSqliteStore::open(
    &store_path(directory),
    &StaticKeySource::new(SymmetricKey::from_bytes(KEY)),
  )
  .unwrap();
  let store = Arc::new(store);
  let api = FakeServer::new().add_user("Alice");
  let client = PendingEnrollment::new()
    .complete(
      api,
      store.clone(),
      Box::new(SystemClock),
      ClientSettings::default(),
      EnrollmentOptions {
        device_name: "phone".into(),
        secret: EnrollmentSecret::Passphrase(SecretBytes::new(b"passphrase".to_vec())),
        argon2: Argon2Settings {
          memory_kib: 1024,
          iterations: 1,
          parallelism: 1,
        },
        maybe_attestation: None,
      },
    )
    .await
    .unwrap();
  let me = client.user_id();
  drop(client);
  let peer: UserId = id(77);
  let in_first = messages / 2;
  let per_other = (messages - in_first) / (CONVERSATIONS - 1);
  for index in 0..CONVERSATIONS {
    let conversation = conversation_id(index);
    records::put_json(
      store.as_ref(),
      Table::Conversations,
      &conversation.to_string(),
      &conversation_record(conversation, me, peer),
    )
    .unwrap();
    let count = if index == 0 { in_first } else { per_other };
    for start in (0..count).step_by(1_000) {
      write_group(store.as_ref(), || {
        for n in start..(start + 1_000).min(count) {
          store.upsert_message(&message(conversation, peer, index, n))?;
        }
        Ok::<_, teamcraft_core::store::StoreError>(())
      })
      .unwrap();
    }
  }
  let filler = vec![0x5a; MLS_ENTRY_BYTES];
  write_group(store.as_ref(), || {
    for entry in 0..MLS_ENTRIES {
      store.put(Table::MlsStorage, &format!("entry/{entry:032x}"), &filler)?;
    }
    Ok::<_, teamcraft_core::store::StoreError>(())
  })
  .unwrap();
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

fn message(conversation: ConversationId, sender: UserId, index: i64, n: i64) -> StoredMessage {
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
    content: MessageContent::text("A message of the usual length in a long history.", Vec::new()),
    is_edited: false,
    edits: Vec::new(),
    maybe_edit_id: None,
    is_pinned: false,
    maybe_verified_commit: None,
    maybe_server_change: None,
    maybe_unreadable: None,
  }
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

fn store_path(directory: &std::path::Path) -> std::path::PathBuf {
  directory.join(format!("{ACCOUNT}.teamcraft.db"))
}

fn config(directory: &std::path::Path) -> AppConfig {
  AppConfig {
    server_url: UNREACHABLE_SERVER.into(),
    data_dir: directory.display().to_string(),
    account: ACCOUNT.into(),
    database_key: KEY.to_vec(),
    push_gateway_url: None,
    is_memory_constrained: false,
  }
}

struct Quiet;

impl EventListener for Quiet {
  fn on_change(&self) {}
}

struct ValidToken;

impl TokenProvider for ValidToken {
  fn access_token(&self) -> Option<String> {
    Some("token".into())
  }
}

/// Blocks like AppAuth: the first ask takes [`SLOW_REFRESH`].
struct SlowRefresh(Mutex<()>);

impl TokenProvider for SlowRefresh {
  fn access_token(&self) -> Option<String> {
    let _once = self.0.lock().unwrap();
    let (_never, waits) = mpsc::channel::<()>();
    let _ = waits.recv_timeout(SLOW_REFRESH);
    Some("token".into())
  }
}
