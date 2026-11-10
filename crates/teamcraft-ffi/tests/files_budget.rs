//! What the Files view costs on a large workspace (issue #1509), through the API the apps use
//! (robustness.md, "Performance and resources"). A measurement, not a check: run it by hand, with
//! `--release`:
//!
//! `cargo test --release -p teamcraft-ffi --test files_budget -- --ignored --nocapture`
//!
//! 100,000 messages in 300 conversations, half of them in one; every 20th message has a file, every
//! other of those a picture. "Before" reads each conversation's files the way the view did on every
//! 15 s tick (`conversation_files` per conversation); "after" is the indexed page.

#![allow(clippy::print_stderr, reason = "the numbers are what this test is for")]
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::SystemClock;
use teamcraft_core::records;
use teamcraft_core::store::{
  LocalStore, MessageContent, MessageMeta, SealedSqliteStore, StaticKeySource, StoredMessage, Table,
  write_group,
};
use teamcraft_core::{ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_ffi::{AccessToken, AccountState, AppConfig, EventListener, TeamCraft, TokenProvider};
use teamcraft_proto::{
  ChannelModeration, ClientMessageId, ConversationId, ConversationResponse, Id, Snowflake, UserId,
};

const KEY: [u8; 32] = [7; 32];
const ACCOUNT: &str = "account";
const CONVERSATIONS: i64 = 300;
const MESSAGES: i64 = 100_000;
const FILE_EVERY: i64 = 20;
const UNREACHABLE_SERVER: &str = "http://127.0.0.1:9";
const RUNS: usize = 20;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement: run by hand with --release"]
async fn measure_the_files_view() {
  let directory = std::env::temp_dir().join(format!("teamcraft-files-budget-{}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let started = Instant::now();
  seed(&directory, MESSAGES).await;
  eprintln!("seeded {MESSAGES} messages in {:?}", started.elapsed());
  let core = TeamCraft::new(config(&directory), Arc::new(ValidToken), Arc::new(Quiet)).unwrap();
  assert!(matches!(
    core.account_state().await.unwrap(),
    AccountState::Ready { .. }
  ));
  core.start().await.unwrap();
  let conversations: Vec<String> = (0..CONVERSATIONS)
    .map(|index| conversation_id(index).to_string())
    .collect();

  let before = Instant::now();
  let mut files = 0;
  for conversation in &conversations {
    let answer = core
      .query(serde_json::json!({ "kind": "conversation_files", "conversation": conversation }).to_string())
      .await
      .unwrap();
    let answer: serde_json::Value = serde_json::from_str(&answer).unwrap();
    files += answer["files"].as_array().map_or(0, Vec::len);
  }
  eprintln!(
    "before: every conversation's files ({files} files)  {:>8.1} ms",
    ms(before.elapsed())
  );

  for (label, filter, sort, max_files) in [
    ("all, newest, 1 page ", "all", "newest", 60),
    ("all, newest, 3 pages", "all", "newest", 180),
    ("documents, newest   ", "documents", "newest", 60),
    ("media, oldest       ", "media", "oldest", 60),
    ("all, by name        ", "all", "name", 60),
  ] {
    let query = serde_json::json!({
      "kind": "files_page",
      "scope": "everything",
      "filter": filter,
      "sort": sort,
      "max_files": max_files,
    })
    .to_string();
    let first = Instant::now();
    let answer = core.query(query.clone()).await.unwrap();
    let first = first.elapsed();
    let answer: serde_json::Value = serde_json::from_str(&answer).unwrap();
    let shown = answer["files"].as_array().map_or(0, Vec::len);
    assert_eq!(shown, max_files, "a measurement of nothing would look very good");
    let mut kept = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
      let again = Instant::now();
      core.query(query.clone()).await.unwrap();
      kept.push(again.elapsed());
    }
    kept.sort();
    eprintln!(
      "after: {label}  first {:>7.1} ms, kept p50 {:>5.2} ms ({shown} files)",
      ms(first),
      ms(kept[RUNS / 2])
    );
  }
  core.stop().await.unwrap();
  drop(core);
  std::fs::remove_dir_all(&directory).unwrap();
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
    content: match n % FILE_EVERY {
      0 => file_content(index, n),
      _ => MessageContent::text("A message of the usual length in a long history.", Vec::new()),
    },
    is_edited: false,
    edits: Vec::new(),
    maybe_edit_id: None,
    is_pinned: false,
    maybe_verified_commit: None,
    maybe_server_change: None,
    maybe_unreadable: None,
  }
}

/// A text with one file: a picture for every other file, a document otherwise.
fn file_content(index: i64, n: i64) -> MessageContent {
  let media_type = match (n / FILE_EVERY) % 2 {
    0 => "image/jpeg",
    _ => "application/pdf",
  };
  let attachment = serde_json::json!({
    "attachment_id": id::<teamcraft_proto::AttachmentId>(1_000_000_000 + index * 10_000_000 + n),
    "key": "",
    "ciphertext_sha256": "",
    "file_name": format!("file-{n}"),
    "media_type": media_type,
    "size_bytes": 1_000 + n,
  });
  MessageContent::Text {
    body: "Here is the file.".into(),
    mentions: Vec::new(),
    meta: MessageMeta {
      attachments: vec![serde_json::from_value(attachment).unwrap()],
      ..Default::default()
    },
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
  fn access_token(&self) -> AccessToken {
    AccessToken::Token {
      value: "token".into(),
    }
  }
}
