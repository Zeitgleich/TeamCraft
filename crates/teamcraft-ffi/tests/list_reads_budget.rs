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
//! (default 300), `TEAMCRAFT_STORM_SECS` how long (default 20), `TEAMCRAFT_STORM_MESSAGES`
//! messages per conversation (default 400), `TEAMCRAFT_STORM_BIG_CHATS` channels (default 3) with
//! `TEAMCRAFT_STORM_BIG_MESSAGES` (default 90,000) instead, like an imported history. The sync loop finds no server.

#![allow(clippy::print_stderr, reason = "the numbers are what this test is for")]
#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::SystemClock;
use teamcraft_core::records;
use teamcraft_core::store::{
  CommitJournal, LaneStore, LocalStore, MessageContent, SealedSqliteStore, SealedStoreLimits,
  StaticKeySource, StoredMessage, Table, TrackedStore, Unreadable, read_together, write_group,
};
use teamcraft_core::ui_backend::CoreChatBackend;
use teamcraft_core::{ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_ffi::{AccessToken, AccountState, AppConfig, EventListener, TeamCraft, TokenProvider};
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
const DEFAULT_MESSAGES_PER_CONVERSATION: i64 = 400;
/// A few channels with an imported history of this length, as on a real instance.
const DEFAULT_BIG_CHATS: i64 = 3;
const DEFAULT_BIG_MESSAGES: i64 = 90_000;
/// The sync's messages are numbered from here, after any seeded history.
const SYNCED_FROM: i64 = 1_000_000;
const SEED_BATCH: usize = 2_000;
/// The newest messages of each big channel among which some wait for keys.
const WAITING_ROWS: i64 = 2_000;
/// Big channels nobody read on this account yet (an import), with no read position.
const DEFAULT_BIG_NEVER_READ: i64 = 1;
/// As the engine's `GROUP_TURN_MS`: a group runs this long before it ends early for a read.
const GROUP_TURN: Duration = Duration::from_millis(16);
const COLD_PASSES: usize = 20;
const CATCH_UP_EVERY: Duration = Duration::from_millis(250);
const DECRYPTED_PER_BATCH: usize = 25;
const OLDER_PER_BATCH: i64 = 10;
const NEW_PER_BATCH: i64 = 5;
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

/// What one chat list pass costs after the writes a sync makes, each kind on its own, through the
/// core the read thread uses and the same store connection (a second connection's writes don't
/// reach the feed versions the counts are kept under).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement: run by hand with --release"]
async fn measure_a_list_pass_after_each_kind_of_sync_write() {
  let directory = std::env::temp_dir().join(format!("teamcraft-list-pass-{}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let seeded = Instant::now();
  let me = seed(&directory).await;
  eprintln!("seeded {CONVERSATIONS} conversations in {:?}", seeded.elapsed());
  let sealed = Arc::new(
    SealedSqliteStore::open(
      &store_path(&directory),
      &StaticKeySource::new(SymmetricKey::from_bytes(KEY)),
    )
    .unwrap(),
  );
  let store = TrackedStore::new(sealed);
  let backend = CoreChatBackend::new(store.clone(), me, Box::new(SystemClock));
  let spaces: BTreeSet<SpaceId> = (0..SPACES).map(|space| id(900 + space)).collect();
  let pass = |label: &str| {
    let started = Instant::now();
    let listed = backend.listed_conversations(&spaces);
    assert_eq!(listed.len(), CONVERSATIONS as usize);
    eprintln!("{label:<58} {:>9.1} ms", ms(started.elapsed()));
  };
  pass("first pass (counts every conversation, keeps the tallies)");
  pass("nothing changed");
  let mut next = SYNCED_FROM;
  for round in 0..2 {
    eprintln!(
      "-- big channels read {}",
      if round == 0 {
        "up to near the end"
      } else {
        "never (no read position)"
      }
    );
    write(&store, |store| {
      for index in (0..CONVERSATIONS).step_by(usize::try_from(SYNC_SPREAD).unwrap()) {
        store.upsert_message(&message(conversation_id(index), id(500), index, next))?;
      }
      Ok(())
    });
    next += 1;
    pass("new messages in 10 conversations");
    write(&store, |store| {
      for index in 0..big_chats() {
        store.upsert_message(&message(
          conversation_id(index),
          id(500 + index),
          index,
          history_of(index) / 2,
        ))?;
      }
      Ok(())
    });
    pass("an old message written again in each big channel (edit)");
    pass("nothing changed");
    write(&store, |store| {
      for index in 0..big_chats() {
        let mut older = message(conversation_id(index), id(500 + index), index, 0);
        older.maybe_server_id = Some(id(1_000_000_000 + index * 10_000_000 - 1 - next));
        older.client_message_id = ClientMessageId::from_bytes([u8::try_from(next % 200).unwrap(); 16]);
        store.upsert_message(&older)?;
      }
      Ok(())
    });
    next += 1;
    pass("an older message inserted in each big channel (history)");
    pass("nothing changed");
    if round == 0 {
      write(&store, |store| {
        for index in 0..big_chats() {
          let record = channel_record(conversation_id(index), id(900), index, None);
          records::put_json(
            store,
            Table::Conversations,
            &conversation_id(index).to_string(),
            &record,
          )?;
        }
        Ok(())
      });
      pass("read positions of the big channels gone");
    }
  }
  drop(backend);
  drop(store);
  std::fs::remove_dir_all(&directory).unwrap();
}

/// Writes in one group, as the sync loop does.
fn write(store: &Store, writes: impl FnOnce(&Store) -> Result<(), teamcraft_core::CoreError>) {
  write_group(store, || writes(store)).unwrap();
}

type Store = TrackedStore<Arc<SealedSqliteStore>>;

/// The start of issue #1466: the app comes back after a while, the list's counts of its last run
/// kept, and a catch-up starts at once. It decrypts messages that waited for their keys, loads
/// older history into big channels, adds new messages and edits, through the store connection the
/// reads use, while the read thread reads the chat list after every change and the user opens a
/// chat now and then. The numbers: until the list shows, one list pass meanwhile, and how long
/// opening a chat waits behind them (from the tap: a read thread does one thing at a time).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement: run by hand with --release"]
async fn measure_a_start_during_a_catch_up() {
  let directory = std::env::temp_dir().join(format!("teamcraft-start-sync-{}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let seeded = Instant::now();
  let me = seed(&directory).await;
  let store = open_store(&directory);
  let waiting = park_for_keys(&store);
  eprintln!(
    "seeded {CONVERSATIONS} conversations, {} waiting for keys, in {:?}",
    waiting.len(),
    seeded.elapsed()
  );
  let spaces: BTreeSet<SpaceId> = (0..SPACES).map(|space| id(900 + space)).collect();
  {
    let last_run = CoreChatBackend::new(store.clone(), me, Box::new(SystemClock));
    last_run.listed_conversations(&spaces);
  }
  drop(store);

  let store = open_store(&directory);
  let backend = CoreChatBackend::new(store.clone(), me, Box::new(SystemClock));
  let start = Instant::now();
  backend.listed_conversations(&spaces);
  eprintln!(
    "list usable after the start            {:>9.1} ms",
    ms(start.elapsed())
  );

  let length = Duration::from_secs(env_number("TEAMCRAFT_STORM_SECS", 30));
  let is_over = Arc::new(AtomicBool::new(false));
  let catch_up = CatchUp::start(store.clone(), waiting, is_over.clone());
  let mut passes = Vec::new();
  let mut opens = Vec::new();
  let started = Instant::now();
  let mut next_open = started + OPEN_EVERY;
  let mut index = 0;
  while started.elapsed() < length {
    let pass = Instant::now();
    backend.listed_conversations(&spaces);
    passes.push(pass.elapsed());
    if Instant::now() >= next_open {
      let conversation = conversation_id(index % CONVERSATIONS);
      index += 7;
      // As the read thread runs a job: one look at the store.
      let tail = read_together(&store, || {
        Ok::<_, teamcraft_core::store::StoreError>(backend.feed_tail(conversation, None, PAGE as usize))
      })
      .unwrap();
      assert!(!tail.messages.is_empty(), "no messages in a seeded conversation");
      opens.push(next_open.elapsed());
      next_open += OPEN_EVERY;
      // Taps the user made while the thread was busy wait their turn.
      next_open = next_open.max(Instant::now() - OPEN_EVERY);
    }
  }
  is_over.store(true, Ordering::SeqCst);
  let (written, mut groups) = catch_up.join().unwrap();
  groups.sort();
  passes.sort();
  opens.sort();
  eprintln!(
    "list pass during the catch-up          p50 {:>8.1} ms, p95 {:>8.1} ms, p99 {:>8.1} ms, max {:>8.1} ms over {} passes",
    ms(passes[passes.len() / 2]),
    ms(passes[passes.len() * 95 / 100]),
    ms(passes[passes.len() * 99 / 100]),
    ms(passes[passes.len() - 1]),
    passes.len()
  );
  eprintln!(
    "opening a chat, from the tap           p50 {:>8.1} ms, p95 {:>8.1} ms, max {:>8.1} ms over {} opens",
    ms(opens[opens.len() / 2]),
    ms(opens[opens.len() * 95 / 100]),
    ms(opens[opens.len() - 1]),
    opens.len()
  );
  eprintln!(
    "catch-up writes                        {written}, a group held for p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms",
    ms(groups[groups.len() / 2]),
    ms(groups[groups.len() * 95 / 100]),
    ms(groups[groups.len() - 1])
  );
  drop(backend);
  drop(store);
  std::fs::remove_dir_all(&directory).unwrap();
}

/// What a list pass costs when every pass follows another connection's commit (what a read
/// connection of its own would see of the engine's commits): the store drops its caches each time.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement: run by hand with --release"]
async fn measure_a_list_pass_after_another_connections_commit() {
  let directory = std::env::temp_dir().join(format!("teamcraft-cold-pass-{}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let me = seed(&directory).await;
  let store = open_store(&directory);
  let other = open_store(&directory);
  let backend = CoreChatBackend::new(store.clone(), me, Box::new(SystemClock));
  let spaces: BTreeSet<SpaceId> = (0..SPACES).map(|space| id(900 + space)).collect();
  backend.listed_conversations(&spaces);
  let mut warm = Vec::new();
  let mut cold = Vec::new();
  for n in 0..COLD_PASSES {
    let pass = Instant::now();
    backend.listed_conversations(&spaces);
    warm.push(pass.elapsed());
    other
      .put(Table::SyncState, "cold-pass", &n.to_le_bytes())
      .unwrap();
    let pass = Instant::now();
    backend.listed_conversations(&spaces);
    cold.push(pass.elapsed());
  }
  warm.sort();
  cold.sort();
  eprintln!(
    "list pass, caches warm: p50 {:.1} ms; after another connection's commit: p50 {:.1} ms, max {:.1} ms",
    ms(warm[COLD_PASSES / 2]),
    ms(cold[COLD_PASSES / 2]),
    ms(cold[COLD_PASSES - 1])
  );
  drop((backend, store, other));
  std::fs::remove_dir_all(&directory).unwrap();
}

/// Issue #1476: the same passes on a read connection of its own, while the app's writing
/// connection commits between them. The writer stages what it changed, so the reader keeps the rest
/// of its caches: the passes should cost what the warm ones of the test above do, not the cold ones.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement: run by hand with --release"]
async fn measure_a_list_pass_on_a_read_connection_after_the_writers_commit() {
  let directory = std::env::temp_dir().join(format!("teamcraft-lane-pass-{}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let me = seed(&directory).await;
  let key_source = StaticKeySource::new(SymmetricKey::from_bytes(KEY));
  let journal = CommitJournal::new();
  let writer = Arc::new(SealedSqliteStore::open(&store_path(&directory), &key_source).unwrap());
  writer.publish_commits(Arc::clone(&journal));
  let reader = SealedSqliteStore::open_reader(
    &store_path(&directory),
    &key_source,
    SealedStoreLimits::default(),
    journal,
  )
  .unwrap();
  let engine = TrackedStore::new(LaneStore::writer(Arc::clone(&writer)));
  let lane = engine.sharing(LaneStore::reader(Arc::clone(&writer), Arc::new(reader)));
  let backend = CoreChatBackend::new(lane.clone(), me, Box::new(SystemClock));
  let spaces: BTreeSet<SpaceId> = (0..SPACES).map(|space| id(900 + space)).collect();
  read_together(&lane, || {
    backend.listed_conversations(&spaces);
    Ok::<_, teamcraft_core::store::StoreError>(())
  })
  .unwrap();
  let (mut warm, mut after_record, mut after_message) = (Vec::new(), Vec::new(), Vec::new());
  let pass = |into: &mut Vec<Duration>| {
    let started = Instant::now();
    read_together(&lane, || {
      backend.listed_conversations(&spaces);
      Ok::<_, teamcraft_core::store::StoreError>(())
    })
    .unwrap();
    into.push(started.elapsed());
  };
  for n in 0..COLD_PASSES {
    pass(&mut warm);
    engine
      .put(Table::SyncState, "lane-pass", &n.to_le_bytes())
      .unwrap();
    pass(&mut after_record);
    let index = i64::try_from(n).unwrap() % CONVERSATIONS;
    engine
      .upsert_message(&message(
        conversation_id(index),
        id(500 + index % PEOPLE),
        index,
        SYNCED_FROM + i64::try_from(n).unwrap(),
      ))
      .unwrap();
    pass(&mut after_message);
  }
  for passes in [&mut warm, &mut after_record, &mut after_message] {
    passes.sort();
  }
  eprintln!(
    "list pass on a read connection, warm: p50 {:.1} ms; after the writer's commit of a record: p50 {:.1} ms, max {:.1} ms; of a message: p50 {:.1} ms, max {:.1} ms",
    ms(warm[COLD_PASSES / 2]),
    ms(after_record[COLD_PASSES / 2]),
    ms(after_record[COLD_PASSES - 1]),
    ms(after_message[COLD_PASSES / 2]),
    ms(after_message[COLD_PASSES - 1])
  );
  drop((backend, lane, engine, writer));
  std::fs::remove_dir_all(&directory).unwrap();
}

/// Messages of the big channels that came before their keys: every fourth of the newest
/// [`WAITING_ROWS`] of each, stored unreadable. A never-read big channel has no read position.
fn park_for_keys(store: &Store) -> Vec<StoredMessage> {
  let mut waiting = Vec::new();
  write(store, |store| {
    for index in 0..big_chats() {
      let history = history_of(index);
      for n in (history.saturating_sub(WAITING_ROWS)..history).step_by(4) {
        let readable = message(conversation_id(index), id(500 + (index + n) % PEOPLE), index, n);
        let mut parked = readable.clone();
        parked.content = MessageContent::Unknown;
        parked.maybe_unreadable = Some(Unreadable::Waiting { since_ms: 1 });
        store.upsert_message(&parked)?;
        waiting.push(readable);
      }
      if index < env_number("TEAMCRAFT_STORM_BIG_NEVER_READ", DEFAULT_BIG_NEVER_READ) {
        let record = channel_record(conversation_id(index), id(900), index, None);
        records::put_json(
          store,
          Table::Conversations,
          &conversation_id(index).to_string(),
          &record,
        )?;
      }
    }
    Ok(())
  });
  waiting
}

/// What a catch-up writes, a batch every [`CATCH_UP_EVERY`]: keys came for some waiting messages,
/// a page of older history of a big channel, new messages, edits.
struct CatchUp;

impl CatchUp {
  fn start(
    store: Store,
    mut waiting: Vec<StoredMessage>,
    is_over: Arc<AtomicBool>,
  ) -> std::thread::JoinHandle<(usize, Vec<Duration>)> {
    std::thread::spawn(move || {
      let mut written = 0;
      let mut batch: i64 = 0;
      let mut groups = Vec::new();
      while !is_over.load(Ordering::SeqCst) {
        let mut page: Vec<StoredMessage> = waiting.drain(..DECRYPTED_PER_BATCH.min(waiting.len())).collect();
        let big = batch % big_chats();
        for k in 0..OLDER_PER_BATCH {
          let mut older = message(conversation_id(big), id(500), big, 0);
          older.maybe_server_id = Some(id(
            1_000_000_000 + big * 10_000_000 - 1 - batch * OLDER_PER_BATCH - k,
          ));
          let mut client = [0x77; 16];
          client[..8].copy_from_slice(&(batch * OLDER_PER_BATCH + k).to_be_bytes());
          older.client_message_id = ClientMessageId::from_bytes(client);
          older.sent_at_ms = 1_600_000_000_000 - batch * OLDER_PER_BATCH - k;
          page.push(older);
        }
        for k in 0..NEW_PER_BATCH {
          let index = (batch * NEW_PER_BATCH + k) % CONVERSATIONS;
          page.push(message(
            conversation_id(index),
            id(500),
            index,
            SYNCED_FROM + batch,
          ));
        }
        let edited = batch % CONVERSATIONS;
        let mut edit = message(
          conversation_id(edited),
          id(500 + edited % PEOPLE),
          edited,
          history_of(edited) - 2,
        );
        edit.is_edited = true;
        page.push(edit);
        // As the engine receives a page: in groups that end early while a read waits.
        let mut page = page.into_iter().peekable();
        while page.peek().is_some() {
          let group = Instant::now();
          write(&store, |store| {
            for message in page.by_ref() {
              store.upsert_message(&message)?;
              written += 1;
              if group.elapsed() >= GROUP_TURN && store.is_awaited() {
                break;
              }
            }
            Ok(())
          });
          groups.push(group.elapsed());
        }
        batch += 1;
        std::thread::sleep(CATCH_UP_EVERY);
      }
      (written, groups)
    })
  }
}

fn open_store(directory: &std::path::Path) -> Store {
  TrackedStore::new(Arc::new(
    SealedSqliteStore::open(
      &store_path(directory),
      &StaticKeySource::new(SymmetricKey::from_bytes(KEY)),
    )
    .unwrap(),
  ))
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
      // After every seeded history, the longest included.
      let mut next = SYNCED_FROM;
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

/// Seeds the store and answers the account's user.
async fn seed(directory: &std::path::Path) -> UserId {
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
    let maybe_last_read = Some(message_id(index, history_of(index) - unread - 1));
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
    let history = history_of(index);
    for start in (0..history).step_by(SEED_BATCH) {
      write_group(store.as_ref(), || {
        for n in start..(start + SEED_BATCH as i64).min(history) {
          let sender = people[usize::try_from((index + n) % PEOPLE).unwrap()].id;
          store.upsert_message(&message(conversation, sender, index, n))?;
        }
        Ok::<_, teamcraft_core::store::StoreError>(())
      })
      .unwrap();
    }
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
      Ok::<_, teamcraft_core::CoreError>(())
    })
    .unwrap();
  }
  me
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

/// How many messages conversation `index` holds: the first `TEAMCRAFT_STORM_BIG_CHATS` channels
/// have a long imported history (`TEAMCRAFT_STORM_BIG_MESSAGES`), the others
/// `TEAMCRAFT_STORM_MESSAGES`.
fn history_of(index: i64) -> i64 {
  if index < big_chats() {
    env_number("TEAMCRAFT_STORM_BIG_MESSAGES", DEFAULT_BIG_MESSAGES).min(SYNCED_FROM)
  } else {
    env_number("TEAMCRAFT_STORM_MESSAGES", DEFAULT_MESSAGES_PER_CONVERSATION).min(SYNCED_FROM)
  }
}

fn big_chats() -> i64 {
  env_number("TEAMCRAFT_STORM_BIG_CHATS", DEFAULT_BIG_CHATS)
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
  fn access_token(&self) -> AccessToken {
    AccessToken::Token {
      value: "token".into(),
    }
  }
}
