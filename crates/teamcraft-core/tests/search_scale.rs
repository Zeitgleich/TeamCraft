//! Search on a large workspace (issue #154, docs/performance.md, "Search on a large workspace"):
//! what a search costs in time and memory when the local store holds many messages.
//!
//! Its own test binary: the counting allocator sees every allocation of the process, so no other
//! test may run beside it.
//!
//! `TEAMCRAFT_SEARCH_MESSAGES` sets the workspace's size (default 20,000 messages in 300
//! conversations, half of them in one channel; docs/performance.md has the numbers for 100,000,
//! measured with `--release`):
//!
//! ```sh
//! TEAMCRAFT_SEARCH_MESSAGES=100000 cargo test --release -p teamcraft-core --all-features \
//!   --test search_scale -- --nocapture
//! ```
#![cfg(all(feature = "sqlcipher", feature = "ui", not(target_arch = "wasm32")))]
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use peak_alloc::PeakAlloc;
use rand_core::OsRng;
use teamcraft_client_api::backend::SearchBackend;
use teamcraft_client_api::search::parse_query;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::search::{SearchQuery, SearchService};
use teamcraft_core::search_index::{DEFAULT_STEP_MESSAGES, SearchIndexer};
use teamcraft_core::store::{
  DEFAULT_MESSAGE_CACHE_BYTES, LocalStore, MessageContent, SqlCipherStore, StaticKeySource, StoredMessage,
  Table, TrackedStore, write_group,
};
use teamcraft_core::ui_backend::CoreChatBackend;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{
  ChannelModeration, ClientMessageId, ConversationId, ConversationKind, ConversationResponse,
  ConversationVisibility, HistoryPolicy, Id, MessageId, Snowflake, UserId, UserKind, UserSummary,
};

#[global_allocator]
static ALLOCATOR: PeakAlloc = PeakAlloc;

const MIB: usize = 1024 * 1024;
const CONVERSATIONS: i64 = 300;
const DEFAULT_MESSAGES: usize = 20_000;
/// Words of the made-up vocabulary the history is written in, besides the planted ones.
const VOCABULARY: usize = 5_000;
/// One message in this many mentions the budget.
const BUDGET_EVERY: i64 = 1_000;
/// Searches per query for the timings.
const RUNS: usize = 7;
/// Generous for a debug build on a busy CI runner.
const MAX_SEARCH: Duration = Duration::from_secs(5);

type Store = TrackedStore<Arc<SqlCipherStore>>;

#[test]
fn search_on_a_large_workspace() {
  let messages: usize = std::env::var("TEAMCRAFT_SEARCH_MESSAGES")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(DEFAULT_MESSAGES);
  // `TEAMCRAFT_SEARCH_DIR` puts the store elsewhere (a faster disk than the target directory's).
  let directory = std::env::var_os("TEAMCRAFT_SEARCH_DIR")
    .map_or_else(
      || std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")),
      std::path::PathBuf::from,
    )
    .join(format!("search-scale-{}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let path = directory.join("store.db");
  let key = SymmetricKey::generate(&mut OsRng);
  let me: UserId = id(1);

  let filled = Instant::now();
  {
    let store = TrackedStore::new(Arc::new(
      SqlCipherStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap(),
    ));
    fill(&store, me, messages);
  }
  let fill_time = filled.elapsed();

  // The desktop's store: SQLCipher with the message cache of the conversations read last.
  let store: Store = TrackedStore::with_message_cache(
    Arc::new(SqlCipherStore::open(&path, &StaticKeySource::new(key)).unwrap()),
    DEFAULT_MESSAGE_CACHE_BYTES,
  );
  let budget_hits = messages as i64 / BUDGET_EVERY;
  let mut lines = vec![format!(
    "{messages} messages in {CONVERSATIONS} conversations, filled in {fill_time:?}"
  )];

  // The local index (issue #154), built as the upkeep does: one bounded step per tick.
  let mut indexer = SearchIndexer::new();
  let before = ALLOCATOR.current_usage();
  ALLOCATOR.reset_peak_usage();
  let started = Instant::now();
  let (mut steps, mut slowest) = (0, Duration::ZERO);
  loop {
    let step_started = Instant::now();
    let step = indexer.step(&store, DEFAULT_STEP_MESSAGES).unwrap();
    slowest = slowest.max(step_started.elapsed());
    steps += 1;
    if step.is_done {
      break;
    }
  }
  let index_bytes: usize = store
    .list(Table::SearchIndex)
    .unwrap()
    .iter()
    .map(|(_, value)| value.len())
    .sum();
  lines.push(format!(
    "index built in {:?} ({steps} steps, slowest {slowest:?}), {:.1} MiB at its peak; {:.1} MiB in the store",
    started.elapsed(),
    ALLOCATOR.peak_usage().saturating_sub(before) as f64 / MIB as f64,
    index_bytes as f64 / MIB as f64
  ));
  // New messages: a step indexes only them.
  write_group(&store, || {
    for n in 0..100 {
      store.upsert_message(&message(
        conversation(1).id,
        1,
        1_000_000 + n,
        me,
        "a late budget review".into(),
      ))?;
    }
    Ok::<_, teamcraft_core::store::StoreError>(())
  })
  .unwrap();
  let started = Instant::now();
  let step = indexer.step(&store, DEFAULT_STEP_MESSAGES).unwrap();
  lines.push(format!(
    "100 new messages indexed in {:?} ({} records)",
    started.elapsed(),
    step.handled
  ));
  assert_eq!(step.handled, 100);
  // Idle: nothing waits.
  assert!(indexer.step(&store, DEFAULT_STEP_MESSAGES).unwrap().is_done);
  let budget_hits = budget_hits + 100;

  let mut service = SearchService::new();
  let before = ALLOCATOR.current_usage();
  for (label, terms, expected) in [
    ("rare word", vec!["budget".to_string()], Some(budget_hits)),
    ("rare word, typo", vec!["budgett".to_string()], Some(budget_hits)),
    ("common word", vec![word(0)], None),
    (
      "two words",
      vec!["budget".to_string(), "review".to_string()],
      Some(budget_hits),
    ),
  ] {
    let query = SearchQuery {
      terms,
      ..SearchQuery::default()
    };
    let (timings, peak, found) = measure(|| service.search(&store, &query).unwrap().hits.len());
    if let Some(expected) = expected {
      assert_eq!(found as i64, expected, "{label}");
    }
    lines.push(format!(
      "core search, {label} ({found} hits): {}, {:.1} MiB at its peak",
      summary(&timings),
      peak as f64 / MIB as f64
    ));
    assert!(
      timings[timings.len() / 2] < MAX_SEARCH,
      "{label} took {timings:?}"
    );
  }
  lines.push(format!(
    "core search keeps {:.1} MiB between searches (the index read from the store)",
    ALLOCATOR.current_usage().saturating_sub(before) as f64 / MIB as f64
  ));

  let backend = CoreChatBackend::new(store.clone(), me, Box::new(ManualClock::new(0)));
  for (label, raw) in [
    ("rare word", "budget".to_string()),
    ("common word", word(0)),
    ("rare word in a channel", "budget in:planning".to_string()),
  ] {
    let parsed = parse_query(&raw);
    let (timings, peak, found) = measure(|| backend.search(&parsed).total);
    lines.push(format!(
      "UI search, {label} ({found} hits): {}, {:.1} MiB at its peak",
      summary(&timings),
      peak as f64 / MIB as f64
    ));
    assert!(
      timings[timings.len() / 2] < MAX_SEARCH,
      "{label} took {timings:?}"
    );
  }

  println!("{}", lines.join("\n"));
  drop(backend);
  drop(store);
  std::fs::remove_dir_all(&directory).unwrap();
}

/// Runs `search` [`RUNS`] times: the sorted timings, the heap's peak above where it started, and
/// what the last run returned.
fn measure(mut search: impl FnMut() -> usize) -> (Vec<Duration>, usize, usize) {
  let before = ALLOCATOR.current_usage();
  ALLOCATOR.reset_peak_usage();
  let mut timings = Vec::new();
  let mut found = 0;
  for _ in 0..RUNS {
    let started = Instant::now();
    found = search();
    timings.push(started.elapsed());
  }
  let peak = ALLOCATOR.peak_usage().saturating_sub(before);
  let first = timings[0];
  timings.sort();
  // The first (cold) run is reported on its own.
  timings.insert(0, first);
  (timings, peak, found)
}

/// "first X, median Y, slowest Z" of [`measure`]'s timings.
fn summary(timings: &[Duration]) -> String {
  let sorted = &timings[1..];
  format!(
    "first {:?}, median {:?}, slowest {:?}",
    timings[0],
    sorted[sorted.len() / 2],
    sorted[sorted.len() - 1]
  )
}

/// `messages` in [`CONVERSATIONS`] channels, half of them in the first ("planning").
fn fill(store: &Store, me: UserId, messages: usize) {
  let users = [(me, "alice"), (id::<UserId>(2), "bob")];
  let summaries: Vec<UserSummary> = users
    .iter()
    .map(|(user, name)| UserSummary {
      id: *user,
      maybe_username: Some((*name).into()),
      display_name: (*name).into(),
      kind: UserKind::Member,
      maybe_email: None,
      maybe_organization: None,
      maybe_has_devices: None,
      is_deactivated: false,
      is_deleted: false,
    })
    .collect();
  write_group(store, || {
    for summary in &summaries {
      store.put(
        Table::Users,
        &summary.id.to_string(),
        &serde_json::to_vec(summary).unwrap(),
      )?;
    }
    for n in 0..CONVERSATIONS {
      let conversation = conversation(n);
      let name = if n == 0 {
        "planning".to_string()
      } else {
        format!("channel-{n}")
      };
      store.put(
        Table::Conversations,
        &conversation.id.to_string(),
        &serde_json::to_vec(&ConversationResponse {
          maybe_name: Some(name),
          ..conversation
        })
        .unwrap(),
      )?;
    }
    Ok::<_, teamcraft_core::store::StoreError>(())
  })
  .unwrap();
  let messages = i64::try_from(messages).unwrap();
  let in_channel = messages / 2;
  let per_other = (messages - in_channel) / (CONVERSATIONS - 1);
  let mut random = 0x9E37_79B9_7F4A_7C15_u64;
  let mut written = 0;
  for n in 0..CONVERSATIONS {
    let count = if n == 0 { in_channel } else { per_other };
    for start in (0..count).step_by(1_000) {
      write_group(store, || {
        for at in start..(start + 1_000).min(count) {
          let sender = users[usize::try_from(at % 2).unwrap()].0;
          let body = body(&mut random, written % BUDGET_EVERY == 0);
          store.upsert_message(&message(conversation(n).id, n, at, sender, body))?;
          written += 1;
        }
        Ok::<_, teamcraft_core::store::StoreError>(())
      })
      .unwrap();
    }
  }
}

/// About a hundred bytes of made-up words, the common ones more often; `has_budget` plants
/// "budget review" in it.
fn body(random: &mut u64, has_budget: bool) -> String {
  let words = 8 + next(random) % 10;
  let mut body: Vec<String> = (0..words)
    .map(|_| {
      // Skewed towards the first words, as natural language is.
      let unit = (next(random) % 1_000_000) as f64 / 1_000_000.0;
      word((unit * unit * unit * VOCABULARY as f64) as usize)
    })
    .collect();
  if has_budget {
    body.insert(3, "budget review".into());
  }
  if next(random).is_multiple_of(20) {
    body.push("https://example.com/page".into());
  }
  body.join(" ")
}

/// The `n`th word of the made-up vocabulary: two to four syllables, never "budget" or "review".
fn word(n: usize) -> String {
  const SYLLABLES: [&str; 24] = [
    "ka", "lo", "mi", "ne", "su", "ta", "ri", "po", "de", "fa", "gu", "hi", "jo", "ke", "lu", "ma", "no",
    "pe", "qui", "ra", "si", "to", "vu", "we",
  ];
  let mut rest = n;
  let mut word = String::new();
  for _ in 0..2 + n % 3 {
    word.push_str(SYLLABLES[rest % SYLLABLES.len()]);
    rest /= SYLLABLES.len();
  }
  word.push_str(SYLLABLES[(n * 7) % SYLLABLES.len()]);
  word
}

fn next(random: &mut u64) -> u64 {
  // xorshift64: the same history on every run.
  *random ^= *random << 13;
  *random ^= *random >> 7;
  *random ^= *random << 17;
  *random
}

fn message(conversation: ConversationId, index: i64, n: i64, sender: UserId, body: String) -> StoredMessage {
  let mut client_message_id = [0u8; 16];
  client_message_id[..8].copy_from_slice(&index.to_be_bytes());
  client_message_id[8..].copy_from_slice(&n.to_be_bytes());
  StoredMessage {
    conversation_id: conversation,
    client_message_id: ClientMessageId::from_bytes(client_message_id),
    maybe_server_id: Some(id::<MessageId>(1_000_000_000 + index * 10_000_000 + n)),
    sender_user_id: sender,
    maybe_thread_id: None,
    sent_at_ms: 1_700_000_000_000 + n * 60_000,
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

fn conversation(n: i64) -> ConversationResponse {
  ConversationResponse {
    id: id(1_000_000 + n),
    maybe_space_id: None,
    kind: ConversationKind::Channel,
    maybe_name: None,
    maybe_topic: None,
    maybe_description: None,
    visibility: ConversationVisibility::Private,
    history_policy: HistoryPolicy::Full,
    maybe_category_id: None,
    is_archived: false,
    moderation: ChannelModeration::default(),
    is_integration: false,
    contains_guests: false,
    maybe_organizer_user_id: None,
    maybe_my_permissions: None,
    maybe_importer_user_id: None,
    maybe_imported_through: None,
    maybe_my_membership: None,
    participants: Vec::new(),
    maybe_image: None,
  }
}

fn id<T: Id>(raw: i64) -> T {
  T::from_snowflake(Snowflake::new(raw).unwrap())
}
