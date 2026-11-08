//! Opening a chat while the store changes fast (a sync after the app was away), on a workspace the
//! size of a busy phone's: 80 conversations in spaces and DMs, unread messages, mentions (issue
//! #1466). Each change makes several screens read the whole chat list; a screen that moves on
//! cancels its read. The numbers: what one list pass costs, and how long opening a chat waits for
//! its messages meanwhile (robustness.md: channel switch p95 ≤ 150 ms on mobile). A measurement,
//! not a check: run it by hand, with `--release`, best on a phone:
//!
//! `cargo test --release -p teamcraft-ffi --test list_reads_budget -- --ignored --nocapture`
//!
//! `TEAMCRAFT_STORM_READERS` list reads per change (default 3: the chat list, the open
//! conversation's rights, the read watcher), `TEAMCRAFT_STORM_TICK_MS` time between changes
//! (default 300), `TEAMCRAFT_STORM_SECS` how long (default 20). The sync loop finds no server.

#![allow(clippy::print_stderr, reason = "the numbers are what this test is for")]
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
  ChannelModeration, ClientMessageId, ConversationId, ConversationResponse, Id, JoinPolicy, MessageId,
  Snowflake, SpaceId, SpaceResponse, SpaceRole, UserId, UserSummary,
};

const KEY: [u8; 32] = [7; 32];
const ACCOUNT: &str = "account";
const SPACES: i64 = 4;
const CHANNELS_PER_SPACE: i64 = 15;
const DMS: i64 = 20;
const CONVERSATIONS: i64 = SPACES * CHANNELS_PER_SPACE + DMS;
const PEOPLE: i64 = 120;
const MEMBERS_PER_CHANNEL: i64 = 25;
const MESSAGES_PER_CONVERSATION: i64 = 400;
/// Every other conversation has this many unread at its end, some of them mentioning the user.
const UNREAD: i64 = 30;
const PAGE: u32 = 50;
const IDLE_PASSES: usize = 5;
/// The sync writes into every this many-th conversation per batch.
const SYNC_SPREAD: i64 = 8;
const OPEN_EVERY: Duration = Duration::from_millis(700);
const UNREACHABLE_SERVER: &str = "http://127.0.0.1:9";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement: run by hand with --release"]
async fn measure_opening_a_chat_during_a_change_storm() {
  let directory = std::env::temp_dir().join(format!("teamcraft-list-reads-{}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let readers = env_number("TEAMCRAFT_STORM_READERS", 3);
  let tick = Duration::from_millis(env_number("TEAMCRAFT_STORM_TICK_MS", 300));
  let length = Duration::from_secs(env_number("TEAMCRAFT_STORM_SECS", 20));
  let seeded = Instant::now();
  seed(&directory).await;
  eprintln!("seeded {CONVERSATIONS} conversations in {:?}", seeded.elapsed());

  let core = TeamCraft::new(config(&directory), Arc::new(ValidToken), Arc::new(Quiet)).unwrap();
  assert!(matches!(
    core.account_state().await.unwrap(),
    AccountState::Ready { .. }
  ));
  core.start().await.unwrap();
  let listed = core.conversations().await.unwrap();
  // A measurement of nothing would look very good.
  assert_eq!(
    listed.len(),
    CONVERSATIONS as usize,
    "the seeded conversations are not what the app reads"
  );
  assert!(
    listed.iter().any(|item| item.unread_count > 0),
    "nothing unread was seeded"
  );

  let mut passes = Vec::with_capacity(IDLE_PASSES);
  for _ in 0..IDLE_PASSES {
    let pass = Instant::now();
    core.conversations().await.unwrap();
    passes.push(pass.elapsed());
  }
  passes.sort();
  eprintln!(
    "one chat list pass, idle: p50 {:.1} ms, max {:.1} ms",
    ms(passes[IDLE_PASSES / 2]),
    ms(passes[IDLE_PASSES - 1])
  );

  eprintln!("-- change storm: {readers} list reads per change, every {tick:?}, for {length:?}");
  let storm = Storm::run(core.clone(), &directory, readers, tick, length).await;
  storm.report();
  core.stop().await.unwrap();
  drop(core);
  std::fs::remove_dir_all(&directory).unwrap();
}

/// What a storm of changes did to opening a chat.
struct Storm {
  opens: Vec<Duration>,
  answered_reads: usize,
  cancelled_reads: usize,
  /// Messages the sync wrote meanwhile.
  synced: i64,
}

/// What a sync writes while the storm runs: every tick a batch of new messages into some
/// conversations, through a connection of its own, as the engine thread's writes reach the store
/// while the read thread reads. Each batch changes those conversations' counts.
struct Sync;

impl Sync {
  fn start(
    directory: &std::path::Path,
    tick: Duration,
    is_over: Arc<AtomicBool>,
  ) -> std::thread::JoinHandle<i64> {
    let path = store_path(directory);
    std::thread::spawn(move || {
      let store =
        SealedSqliteStore::open(&path, &StaticKeySource::new(SymmetricKey::from_bytes(KEY))).unwrap();
      let mut next = MESSAGES_PER_CONVERSATION;
      let mut written = 0;
      while !is_over.load(Ordering::SeqCst) {
        write_group(&store, || {
          for index in (next % SYNC_SPREAD..CONVERSATIONS).step_by(usize::try_from(SYNC_SPREAD).unwrap()) {
            store.upsert_message(&message(
              conversation_id(index),
              id(500 + index % PEOPLE),
              index,
              next,
            ))?;
            written += 1;
          }
          Ok::<_, teamcraft_core::store::StoreError>(())
        })
        .unwrap();
        next += 1;
        std::thread::sleep(tick);
      }
      written
    })
  }
}

impl Storm {
  /// Every `tick` a change: the readers of the last one that haven't got their answer give up (a
  /// screen's effect restarts) and `readers` new ones ask. The chat list's own refresh reads back
  /// to back, never cancelled. Meanwhile a chat is opened every [`OPEN_EVERY`].
  async fn run(
    core: Arc<TeamCraft>,
    directory: &std::path::Path,
    readers: usize,
    tick: Duration,
    length: Duration,
  ) -> Self {
    let is_over = Arc::new(AtomicBool::new(false));
    let sync = Sync::start(directory, tick, is_over.clone());
    let answered = Arc::new(AtomicUsize::new(0));
    let list = tokio::spawn({
      let (core, is_over, answered) = (core.clone(), is_over.clone(), answered.clone());
      async move {
        while !is_over.load(Ordering::SeqCst) {
          core.conversations().await.unwrap();
          answered.fetch_add(1, Ordering::SeqCst);
        }
      }
    });
    let opener = tokio::spawn({
      let (core, is_over) = (core.clone(), is_over.clone());
      async move {
        let mut opens = Vec::new();
        let mut index = 0;
        while !is_over.load(Ordering::SeqCst) {
          tokio::time::sleep(OPEN_EVERY).await;
          let conversation = conversation_id(index % CONVERSATIONS).to_string();
          index += 7;
          let open = Instant::now();
          core.message_tail(conversation, None, 0, PAGE).await.unwrap();
          opens.push(open.elapsed());
        }
        opens
      }
    });
    let started = Instant::now();
    let mut waiting: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut cancelled_reads = 0;
    // The list's own refresh is one of the readers.
    let others = readers.saturating_sub(1);
    while started.elapsed() < length {
      for read in waiting.drain(..) {
        if !read.is_finished() {
          read.abort();
          cancelled_reads += 1;
        }
      }
      for _ in 0..others {
        let (core, answered) = (core.clone(), answered.clone());
        waiting.push(tokio::spawn(async move {
          core.conversations().await.unwrap();
          answered.fetch_add(1, Ordering::SeqCst);
        }));
      }
      tokio::time::sleep(tick).await;
    }
    is_over.store(true, Ordering::SeqCst);
    for read in waiting {
      read.abort();
    }
    let opens = opener.await.unwrap();
    let synced = sync.join().unwrap();
    list.await.unwrap();
    Self {
      opens,
      answered_reads: answered.load(Ordering::SeqCst),
      cancelled_reads,
      synced,
    }
  }

  fn report(mut self) {
    self.opens.sort();
    let count = self.opens.len();
    assert!(count > 0, "no chat was opened");
    eprintln!(
      "opening a chat: p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms over {count} opens (budget p95 150 ms)",
      ms(self.opens[count / 2]),
      ms(self.opens[count * 95 / 100]),
      ms(self.opens[count - 1])
    );
    eprintln!(
      "list reads answered {}, given up by their screen {}, messages synced {}",
      self.answered_reads, self.cancelled_reads, self.synced
    );
  }
}

async fn seed(directory: &std::path::Path) {
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
  let people: Vec<UserSummary> = (0..PEOPLE)
    .map(|n| person(id(500 + n), &format!("Person {n}")))
    .collect();
  write_group(store.as_ref(), || {
    for user in &people {
      records::put_json(store.as_ref(), Table::Users, &user.id.to_string(), user)?;
    }
    for space in 0..SPACES {
      let space = space_record(id(900 + space));
      records::put_json(store.as_ref(), Table::Spaces, &space.id.to_string(), &space)?;
    }
    Ok::<_, teamcraft_core::CoreError>(())
  })
  .unwrap();
  for index in 0..CONVERSATIONS {
    let conversation = conversation_id(index);
    let peer = people[usize::try_from(index % PEOPLE).unwrap()].id;
    let unread = if index % 2 == 0 { 0 } else { UNREAD };
    let maybe_last_read = Some(message_id(index, MESSAGES_PER_CONVERSATION - unread - 1));
    let record = if index < SPACES * CHANNELS_PER_SPACE {
      channel_record(
        conversation,
        id(900 + index / CHANNELS_PER_SPACE),
        index,
        maybe_last_read,
      )
    } else {
      dm_record(conversation, me, peer, maybe_last_read)
    };
    write_group(store.as_ref(), || {
      records::put_json(store.as_ref(), Table::Conversations, &conversation.to_string(), &record)?;
      let members: Vec<_> = (0..MEMBERS_PER_CHANNEL)
        .map(|n| serde_json::json!({ "user": people[usize::try_from((index + n) % PEOPLE).unwrap()], "role": "member" }))
        .collect();
      records::put_json(
        store.as_ref(),
        Table::Members,
        &conversation.to_string(),
        &serde_json::json!({ "members": members }),
      )?;
      for n in 0..MESSAGES_PER_CONVERSATION {
        let sender = people[usize::try_from((index + n) % PEOPLE).unwrap()].id;
        store.upsert_message(&message(conversation, sender, index, n))?;
      }
      Ok::<_, teamcraft_core::CoreError>(())
    })
    .unwrap();
  }
}

fn space_record(space: SpaceId) -> SpaceResponse {
  SpaceResponse {
    id: space,
    name: format!("Project {space}"),
    maybe_icon: None,
    maybe_color: None,
    maybe_description: None,
    maybe_image: None,
    join_policy: JoinPolicy::InviteOnly,
    is_archived: false,
    maybe_brand_id: None,
    maybe_my_role: Some(SpaceRole::Member),
    maybe_my_notifications: None,
    member_count: 40,
    can_add_members: false,
    can_invite_guests: false,
    can_manage_space: false,
    can_delete_space: false,
    deleted_channel_count: 0,
    can_create_channels: true,
  }
}

fn channel_record(
  conversation: ConversationId,
  space: SpaceId,
  index: i64,
  maybe_last_read: Option<MessageId>,
) -> serde_json::Value {
  checked(serde_json::json!({
    "id": conversation,
    "space_id": space,
    "kind": "channel",
    "name": format!("channel-{index}"),
    "visibility": "public",
    "history_policy": "from_join",
    "is_archived": false,
    "moderation": ChannelModeration::default(),
    "contains_guests": false,
    "my_membership": membership(maybe_last_read),
  }))
}

fn dm_record(
  conversation: ConversationId,
  me: UserId,
  peer: UserId,
  maybe_last_read: Option<MessageId>,
) -> serde_json::Value {
  checked(serde_json::json!({
    "id": conversation,
    "kind": "dm",
    "visibility": "private",
    "history_policy": "from_join",
    "is_archived": false,
    "moderation": ChannelModeration::default(),
    "contains_guests": false,
    "my_membership": membership(maybe_last_read),
    "participants": [person(me, "Alice"), person(peer, "Sam Doe")],
  }))
}

fn membership(maybe_last_read: Option<MessageId>) -> serde_json::Value {
  serde_json::json!({
    "role": "member",
    "notification_level": "all",
    "is_favorite": false,
    "last_read_message_id": maybe_last_read,
  })
}

/// The store takes any JSON, but a record the app can't decode is skipped when read.
fn checked(record: serde_json::Value) -> serde_json::Value {
  serde_json::from_value::<ConversationResponse>(record.clone()).unwrap();
  record
}

fn person(user: UserId, name: &str) -> UserSummary {
  serde_json::from_value(serde_json::json!({
    "id": user,
    "display_name": name,
    "kind": "member",
    "is_deactivated": false,
  }))
  .unwrap()
}

fn message(conversation: ConversationId, sender: UserId, index: i64, n: i64) -> StoredMessage {
  let mut client_message_id = [0u8; 16];
  client_message_id[..8].copy_from_slice(&index.to_be_bytes());
  client_message_id[8..].copy_from_slice(&n.to_be_bytes());
  let body = if n % 9 == 0 {
    "@Alice could you look at this before the meeting?"
  } else {
    "A message of the usual length in a busy channel."
  };
  StoredMessage {
    conversation_id: conversation,
    client_message_id: ClientMessageId::from_bytes(client_message_id),
    maybe_server_id: Some(message_id(index, n)),
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

fn message_id(index: i64, n: i64) -> MessageId {
  id(1_000_000_000 + index * 10_000_000 + n)
}

fn conversation_id(index: i64) -> ConversationId {
  id(1_000_000 + index)
}

fn id<T: Id>(raw: i64) -> T {
  T::from_snowflake(Snowflake::new(raw).unwrap())
}

fn env_number<T: std::str::FromStr>(name: &str, default: T) -> T {
  std::env::var(name)
    .ok()
    .and_then(|text| text.parse().ok())
    .unwrap_or(default)
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
