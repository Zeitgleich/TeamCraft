//! Browser tests (headless Chromium via wasm-bindgen-test): the encrypted IndexedDB store, the
//! Web Crypto key source and the sync engine's crypto running as wasm. See the crate README for
//! how to run them.
#![cfg(target_arch = "wasm32")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

mod common;

use std::ops::ControlFlow;
use std::rc::Rc;
use std::time::Duration;

use common::{PASSPHRASE, clock, options, settings};
use openmls_traits::OpenMlsProvider as _;
use rand_core::{OsRng, RngCore};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::{Clock, SystemClock};
use teamcraft_core::store::{
  IndexedDbStore, LocalStore, MessageContent, MessageFilter, StoreError, StoredMessage, Table,
  browser_database_key, browser_database_key_without_upgrade, browser_dpop_prover,
  browser_dpop_prover_or_new,
};
use teamcraft_core::{PendingEnrollment, mls_storage};
use teamcraft_crypto::dpop::{DpopExpectation, PublicOrigin, verify_proof};
use teamcraft_crypto::local_store::LocalStoreKeys;
use teamcraft_crypto::push::{open_wakeup, seal_wakeup};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{ClientMessageId, ConversationId, Id, Snowflake, UserId};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// Entries and messages of the schema 2 database: more than one batch of the catalog build.
const USERS: usize = 1000;
const MESSAGES: u8 = 100;

#[wasm_bindgen_test]
async fn store_survives_reopening() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  store.put(Table::Users, "1", b"one").unwrap();
  store.put(Table::Users, "2", b"two").unwrap();
  store.delete(Table::Users, "2").unwrap();
  store.upsert_message(&message(7, 1, "hello")).unwrap();
  store.upsert_message(&message(7, 2, "world")).unwrap();
  store.upsert_message(&message(8, 3, "elsewhere")).unwrap();
  store.delete_messages(id(8)).unwrap();
  store.flush().await.unwrap();
  drop(store);

  let reopened = IndexedDbStore::open(&name, &key).await.unwrap();
  assert_eq!(
    reopened.list(Table::Users).unwrap(),
    vec![("1".to_string(), b"one".to_vec())]
  );
  let bodies: Vec<String> = reopened
    .messages(id(7), MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .map(|message| body(&message))
    .collect();
  assert_eq!(bodies, ["hello", "world"]);
  assert!(reopened.messages(id(8), MessageFilter::All).unwrap().is_empty());
}

#[wasm_bindgen_test]
async fn another_key_cannot_open_the_store() {
  let name = database_name();
  let store = IndexedDbStore::open(&name, &SymmetricKey::generate(&mut OsRng))
    .await
    .unwrap();
  store.put(Table::Account, "account", b"secret").unwrap();
  store.flush().await.unwrap();
  drop(store);

  let result = IndexedDbStore::open(&name, &SymmetricKey::generate(&mut OsRng)).await;
  assert!(matches!(result, Err(StoreError::WrongKey)));
}

/// e2ee.md §9.4: a newer version of the app upgrading the database closes this page's copy; writes
/// after that are not lost silently but reported until the page reloads.
#[wasm_bindgen_test]
async fn a_newer_version_closes_the_store_and_writes_report_it() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  store.put(Table::Users, "1", b"one").unwrap();
  store.flush().await.unwrap();

  open_with_newer_version(&name).await;
  store.put(Table::Users, "2", b"two").unwrap();
  assert!(matches!(store.flush().await, Err(StoreError::Closed)));
  // The cache still answers; the page asks the user to reload.
  assert_eq!(store.get(Table::Users, "2").unwrap(), Some(b"two".to_vec()));
}

/// e2ee.md §9.4: a write the browser refuses (quota) is reported by every flush and kept; once space
/// is free it goes through, and a reopened store has it.
#[wasm_bindgen_test]
async fn a_refused_write_is_reported_and_written_once_there_is_space() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  // Retried with every flush (the backoff has a test of its own).
  store.set_retry_backoff(Duration::ZERO, Duration::ZERO);
  refuse_puts(true);
  store.put(Table::Users, "1", b"one").unwrap();
  assert!(matches!(store.flush().await, Err(StoreError::QuotaExceeded)));
  assert!(matches!(store.flush().await, Err(StoreError::QuotaExceeded)));

  refuse_puts(false);
  store.flush().await.unwrap();
  store.flush().await.unwrap();
  drop(store);
  let reopened = IndexedDbStore::open(&name, &key).await.unwrap();
  assert_eq!(reopened.get(Table::Users, "1").unwrap(), Some(b"one".to_vec()));
}

/// Issue #1201: with the quota full the failed writes wait out a backoff instead of going again with
/// every flush (which then doesn't wait for them), and past the limit the store asks for backpressure
/// until they went through. Nothing is dropped.
#[wasm_bindgen_test]
async fn failed_writes_back_off_and_the_backlog_ends_once_they_went_through() {
  const BACKOFF: Duration = Duration::from_millis(300);
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  store.flush().await.unwrap();
  store.set_max_unsaved_writes(3);
  store.set_retry_backoff(BACKOFF, BACKOFF);

  refuse_puts(true);
  for index in 0..3 {
    store.put(Table::Users, &index.to_string(), b"x").unwrap();
  }
  assert!(matches!(store.flush().await, Err(StoreError::QuotaExceeded)));
  assert!(store.is_backlogged());

  refuse_puts(false);
  store.put(Table::Users, "3", b"x").unwrap();
  assert!(
    matches!(store.flush().await, Err(StoreError::QuotaExceeded)),
    "within the backoff nothing goes again, and the flush says why"
  );
  assert!(store.is_backlogged());
  let other = IndexedDbStore::open_without_upgrade(&name, &key).await.unwrap();
  assert_eq!(other.get(Table::Users, "3").unwrap(), None, "held back too");

  teamcraft_core::platform::sleep(BACKOFF + Duration::from_millis(50)).await;
  store.flush().await.unwrap();
  assert!(!store.is_backlogged());
  let reopened = IndexedDbStore::open_without_upgrade(&name, &key).await.unwrap();
  for index in 0..4 {
    assert_eq!(
      reopened.get(Table::Users, &index.to_string()).unwrap(),
      Some(b"x".to_vec()),
      "{index}"
    );
  }
}

/// A key store that can't be read is an error, never "no key": that would end the session.
#[wasm_bindgen_test]
async fn a_dpop_key_store_that_cannot_be_read_is_an_error() {
  browser_dpop_prover_or_new().await.unwrap();
  // What a version bump would have done to a shared version.
  open_with_newer_version("teamcraft-dpop").await;
  let read = browser_dpop_prover().await;
  assert!(
    read.is_err(),
    "{:?}",
    read.ok().flatten().map(|prover| prover.thumbprint())
  );
  delete_database("teamcraft-dpop").await;
  assert!(browser_dpop_prover().await.unwrap().is_none());
}

/// e2ee.md §3.1.2: the DPoP key stays non-extractable and the same across page loads, and what Web
/// Crypto signs is what the server takes.
#[wasm_bindgen_test]
async fn the_dpop_key_is_kept_and_its_proofs_verify() {
  let origins = [PublicOrigin::parse("https://chat.example.com").unwrap()];
  let prover = browser_dpop_prover_or_new().await.unwrap();
  let again = browser_dpop_prover().await.unwrap().unwrap();
  assert_eq!(prover.thumbprint(), again.thumbprint());
  assert_eq!(
    browser_dpop_prover_or_new().await.unwrap().thumbprint(),
    prover.thumbprint()
  );

  let proof = again
    .proof("GET", "https://chat.example.com/api/v1/me?x=1", Some("token"))
    .await
    .unwrap();
  let now_unix = SystemClock.now_ms() / 1000;
  let bound = prover.thumbprint();
  verify_proof(
    proof.as_str(),
    &DpopExpectation {
      method: "GET",
      path: "/api/v1/me",
      public_origins: &origins,
      now_unix,
      max_age_secs: 60,
      leeway_secs: 60,
      access_token: "token",
      maybe_bound_key: Some(&bound),
    },
  )
  .unwrap();
}

#[wasm_bindgen_test]
async fn browser_key_is_created_once_and_reused() {
  let name = database_name();
  let first = browser_database_key(&name).await.unwrap();
  let second = browser_database_key(&name).await.unwrap();
  assert_eq!(first.expose_secret(), second.expose_secret());
  assert_ne!(
    first.expose_secret(),
    browser_database_key(&database_name())
      .await
      .unwrap()
      .expose_secret()
  );

  // The store opens with it, also after reopening.
  let store = IndexedDbStore::open(&name, &first).await.unwrap();
  store.put(Table::Spaces, "1", b"space").unwrap();
  store.flush().await.unwrap();
  drop(store);
  let key = browser_database_key(&name).await.unwrap();
  let reopened = IndexedDbStore::open(&name, &key).await.unwrap();
  assert_eq!(reopened.get(Table::Spaces, "1").unwrap(), Some(b"space".to_vec()));
}

/// Enrollment (Argon2id, identity and device keys), MLS group setup, content keys and message
/// encryption all run as wasm; the receiver's store is IndexedDB and is read back after reopening.
#[wasm_bindgen_test]
async fn two_browser_clients_exchange_an_encrypted_message() {
  let server = FakeServer::new();
  let alice_api = server.add_user("Alice");
  let mut alice = PendingEnrollment::new()
    .complete(
      alice_api.clone(),
      Rc::new(teamcraft_core::store::MemoryStore::new()),
      clock(),
      settings(),
      options(PASSPHRASE),
    )
    .await
    .unwrap();
  let name = database_name();
  let key = browser_database_key(&name).await.unwrap();
  let bob_store = Rc::new(IndexedDbStore::open(&name, &key).await.unwrap());
  let mut bob = PendingEnrollment::new()
    .complete(
      server.add_user("Bob"),
      bob_store.clone(),
      clock(),
      settings(),
      options(PASSPHRASE),
    )
    .await
    .unwrap();
  let (mut bob_gateway, _) = bob.connect_gateway().await.unwrap();

  let space = alice_api.list_spaces().await.unwrap().spaces[0].id;
  let channel = alice.create_channel(space, "browser").await.unwrap();
  alice.add_member(channel, bob.user_id()).await.unwrap();
  alice.send_text(channel, None, "sealed in wasm").await.unwrap();
  for _ in 0..10 {
    if bob.pump(&mut bob_gateway).await.unwrap() == 0 {
      break;
    }
  }
  assert_eq!(texts(bob_store.as_ref(), channel), ["sealed in wasm"]);

  bob_store.flush().await.unwrap();
  drop(bob);
  drop(bob_store);
  let reopened = IndexedDbStore::open(&name, &key).await.unwrap();
  assert_eq!(texts(&reopened, channel), ["sealed in wasm"]);
}

/// Issue #424: a database of the first schema version opens with this one, and the MLS state a
/// version before 1.0.9 wrote as one record moves to one record per entry, which reopens. The old
/// record stays as a marker that versions before 1.0.9 report as damage instead of reading no state
/// (mls_storage.rs `MOVED_MARKER`).
#[wasm_bindgen_test]
async fn an_older_database_opens_and_its_mls_state_moves_to_entries() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  create_version(&name, 1).await;
  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  let mut legacy = Vec::new();
  for chunk in [b"group".as_slice(), b"state".as_slice()] {
    legacy.extend_from_slice(&u32::try_from(chunk.len()).unwrap().to_be_bytes());
    legacy.extend_from_slice(chunk);
  }
  store.put(Table::MlsStorage, "provider", &legacy).unwrap();
  store.flush().await.unwrap();
  drop(store);

  for _ in 0..2 {
    let store = IndexedDbStore::open(&name, &key).await.unwrap();
    let (provider, _) = mls_storage::load_mls_storage(&store).unwrap();
    assert_eq!(
      provider.storage().values.read().unwrap().get(b"group".as_slice()),
      Some(&b"state".to_vec())
    );
    store.flush().await.unwrap();
    let marker = store.get(Table::MlsStorage, "provider").unwrap().unwrap();
    assert_ne!(marker, legacy, "the old record is replaced");
    assert!(
      marker.starts_with(&[0xff; 4]),
      "its length prefix reaches past the record, so an older version sees damage"
    );
  }
}

/// Issue #424 review, M2: the service worker opens the store at the version it has and never
/// upgrades it, so a push never closes an older page that still owns the store; a page does.
#[wasm_bindgen_test]
async fn the_service_worker_never_upgrades_the_database() {
  let name = database_name();
  create_version(&name, 1).await;
  let key = browser_database_key_without_upgrade(&name).await.unwrap();
  drop(IndexedDbStore::open_without_upgrade(&name, &key).await.unwrap());
  assert_eq!(database_version(&name).await, 1.0);

  let key = browser_database_key(&name).await.unwrap();
  drop(IndexedDbStore::open(&name, &key).await.unwrap());
  assert_eq!(database_version(&name).await, current_version().await);
}

/// Issue #1128: a database of schema version 2 (MLS state per entry, no catalog) with records
/// opens with this version, which upgrades it and builds the catalog in several batches; every
/// record reads back, listed and by key, and a reopened store builds nothing again.
#[wasm_bindgen_test]
async fn a_schema_2_database_upgrades_and_builds_its_catalog() {
  let name = database_name();
  let (key, _) = write_schema_2_database(&name).await;

  let mut seen = Vec::new();
  let store = IndexedDbStore::open_reporting(&name, &key, |percent| {
    seen.push(percent);
    ControlFlow::Continue(())
  })
  .await
  .unwrap();
  assert!(!seen.is_empty(), "the catalog was built, in batches");
  assert_eq!(database_version(&name).await, current_version().await);
  assert_everything_is_there(&store);
  store.flush().await.unwrap();
  drop(store);

  let mut was_called = false;
  let store = IndexedDbStore::open_reporting(&name, &key, |_| {
    was_called = true;
    ControlFlow::Continue(())
  })
  .await
  .unwrap();
  assert!(!was_called, "built once");
  assert_everything_is_there(&store);
}

/// Issue #1128: the service worker on a store whose catalog is not complete (the page that owns it
/// stopped building) reads what it needs, writes without upgrading the database, and what it wrote
/// is in the catalog the page completes next.
#[wasm_bindgen_test]
async fn the_service_worker_works_on_a_store_whose_catalog_is_not_complete() {
  let name = database_name();
  let (key, _) = write_schema_2_database(&name).await;
  let stopped = IndexedDbStore::open_reporting(&name, &key, |_| ControlFlow::Break(())).await;
  assert!(matches!(stopped, Err(StoreError::InUse)));
  let version = database_version(&name).await;
  assert_eq!(version, current_version().await);

  let worker = IndexedDbStore::open_without_upgrade(&name, &key).await.unwrap();
  assert_eq!(
    worker.get(Table::Users, "user 7").unwrap(),
    Some(b"value 7".to_vec())
  );
  assert_eq!(
    worker
      .message(id(1), ClientMessageId::from_bytes([3; 16]))
      .unwrap()
      .map(|found| body(&found)),
    Some("message 3".to_string())
  );
  worker.put(Table::Users, "from the worker", b"pushed").unwrap();
  worker
    .upsert_message(&message(1, 200, "from the worker"))
    .unwrap();
  worker.flush().await.unwrap();
  drop(worker);
  assert_eq!(
    database_version(&name).await,
    version,
    "the worker upgrades nothing"
  );

  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  assert_everything_is_there(&store);
  assert_eq!(
    store.get(Table::Users, "from the worker").unwrap(),
    Some(b"pushed".to_vec())
  );
  assert_eq!(store.list(Table::Users).unwrap().len(), USERS + 1);
  assert_eq!(texts(&store, id(1)).len(), usize::from(MESSAGES) + 1);
  assert!(texts(&store, id(1)).contains(&"from the worker".to_string()));
}

/// Issue #1127: a first catalog build tells how far it is, and one that stops (a reload, another tab
/// taking over) goes on from there when the store opens next; nothing is missing afterwards.
#[wasm_bindgen_test]
async fn a_catalog_build_reports_progress_and_goes_on_where_it_stopped() {
  const MESSAGES: u8 = 200;
  // Batches of 4,096 records (#1509): about four of them.
  const MORE_RECORDS: usize = 14_000;
  let name = database_name();
  let key = browser_database_key(&name).await.unwrap();
  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  store.begin_writes().unwrap();
  for client in 0..MESSAGES {
    store
      .upsert_message(&message(1, client, &format!("message {client}")))
      .unwrap();
  }
  // Enough records for several batches, quickly: an entry costs less than a message.
  for index in 0..MORE_RECORDS {
    store
      .put(Table::Users, &format!("key {index}"), b"value")
      .unwrap();
  }
  store.end_writes().unwrap();
  store.flush().await.unwrap();
  drop(store);
  forget_catalog_state(&name, &key).await;

  let mut seen = Vec::new();
  let stopped = IndexedDbStore::open_reporting(&name, &key, |percent| {
    seen.push(percent);
    ControlFlow::Break(())
  })
  .await;
  assert!(matches!(stopped, Err(StoreError::InUse)));
  let stopped_at = seen[0];
  assert!(stopped_at > 0, "one batch of about four is past the start");

  let mut seen = Vec::new();
  let store = IndexedDbStore::open_reporting(&name, &key, |percent| {
    seen.push(percent);
    ControlFlow::Continue(())
  })
  .await
  .unwrap();
  assert!(!seen.is_empty());
  assert!(
    seen[0] > stopped_at,
    "started over: {seen:?}, stopped at {stopped_at}"
  );
  assert!(seen.windows(2).all(|pair| pair[0] <= pair[1]), "{seen:?}");
  assert_eq!(
    texts(&store, id(1)).len(),
    usize::from(MESSAGES),
    "every message is in the index"
  );
  assert_eq!(store.list(Table::Users).unwrap().len(), MORE_RECORDS);
  store.flush().await.unwrap();
  drop(store);

  let mut was_called = false;
  let store = IndexedDbStore::open_reporting(&name, &key, |_| {
    was_called = true;
    ControlFlow::Continue(())
  })
  .await
  .unwrap();
  assert!(!was_called, "built once");
  assert_eq!(texts(&store, id(1)).len(), usize::from(MESSAGES));
}

#[wasm_bindgen_test]
fn push_wake_up_round_trips_in_wasm() {
  let key = SymmetricKey::generate(&mut OsRng);
  let sealed = seal_wakeup(&key, b"{\"kind\":\"message\"}", &mut OsRng).unwrap();
  assert_eq!(
    open_wakeup(&key, &sealed).unwrap().expose_secret(),
    b"{\"kind\":\"message\"}"
  );
  assert!(open_wakeup(&SymmetricKey::generate(&mut OsRng), &sealed).is_err());
}

/// The text messages of a conversation; system events (a member joined) are left out.
fn texts(store: &impl LocalStore, conversation: ConversationId) -> Vec<String> {
  store
    .messages(conversation, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .filter_map(|message| match message.content {
      MessageContent::Text { body, .. } => Some(body),
      _ => None,
    })
    .collect()
}

fn body(message: &StoredMessage) -> String {
  match &message.content {
    MessageContent::Text { body, .. } => body.clone(),
    other => format!("{other:?}"),
  }
}

fn message(conversation: i64, client: u8, text: &str) -> StoredMessage {
  StoredMessage {
    conversation_id: id(conversation),
    client_message_id: ClientMessageId::from_bytes([client; 16]),
    maybe_server_id: Some(id(100 + i64::from(client))),
    sender_user_id: id::<UserId>(1),
    maybe_thread_id: None,
    sent_at_ms: 1,
    content: MessageContent::Text {
      body: text.into(),
      mentions: Vec::new(),
      meta: Default::default(),
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

/// Issue #840: a batch that aborts (quota) must not be overtaken by the batch after it. With both
/// transactions running, the second committed without the first, and a page dying then left a
/// disk that was no prefix of the writes: the second write without the first.
#[wasm_bindgen_test]
async fn a_batch_after_one_that_aborts_never_commits_without_it() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  store.flush().await.unwrap();
  store.set_retry_backoff(Duration::ZERO, Duration::ZERO);

  abort_next_write_transaction();
  store.put(Table::Users, "first", b"1").unwrap();
  store.write_now();
  store.put(Table::Users, "second", b"2").unwrap();
  store.write_now();
  // Another connection, as a reload would read it; its read comes after the abort.
  let other = IndexedDbStore::open_without_upgrade(&name, &key).await.unwrap();
  let is_first_on_disk = other.get(Table::Users, "first").unwrap().is_some();
  let is_second_on_disk = other.get(Table::Users, "second").unwrap().is_some();
  assert!(
    is_first_on_disk || !is_second_on_disk,
    "the second write reached the disk without the first"
  );

  // Both go through in the end, with nothing else asked for than a flush.
  store.flush().await.unwrap();
  let reopened = IndexedDbStore::open_without_upgrade(&name, &key).await.unwrap();
  assert_eq!(reopened.get(Table::Users, "first").unwrap(), Some(b"1".to_vec()));
  assert_eq!(reopened.get(Table::Users, "second").unwrap(), Some(b"2".to_vec()));
}

/// Issue #840: a record that can't be sealed holds its whole batch back: nothing of it is on disk,
/// the flush says so, and everything goes through once the seal works.
#[cfg(feature = "test-support")]
#[wasm_bindgen_test]
async fn a_record_that_does_not_seal_holds_its_batch_back() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  store.flush().await.unwrap();
  store.set_retry_backoff(Duration::ZERO, Duration::ZERO);

  let is_sealable = std::rc::Rc::new(std::cell::Cell::new(false));
  store.put(Table::Users, "before", b"1").unwrap();
  store
    .put_sealing_when(Table::Users, "unsealable", b"2", is_sealable.clone())
    .unwrap();
  store.put(Table::Users, "after", b"3").unwrap();
  assert!(matches!(store.flush().await, Err(StoreError::Database(_))));
  assert!(store.flush().await.is_err(), "reported until it went through");

  let other = IndexedDbStore::open_without_upgrade(&name, &key).await.unwrap();
  assert_eq!(
    other.get(Table::Users, "before").unwrap(),
    None,
    "none of the batch"
  );
  assert_eq!(
    other.get(Table::Users, "after").unwrap(),
    None,
    "none of the batch"
  );

  is_sealable.set(true);
  store.flush().await.unwrap();
  let other = IndexedDbStore::open_without_upgrade(&name, &key).await.unwrap();
  for (key, value) in [("before", b"1"), ("unsealable", b"2"), ("after", b"3")] {
    assert_eq!(
      other.get(Table::Users, key).unwrap(),
      Some(value.to_vec()),
      "{key}"
    );
  }
}

/// Issue #840: in the browser a panic doesn't unwind, so a group it ended stays open. What the group
/// wrote never reaches the disk, not by the timer, the page going away or a flush, and the flush
/// says why.
#[wasm_bindgen_test]
async fn a_group_a_panic_left_open_never_reaches_the_disk() {
  let name = database_name();
  let key = SymmetricKey::generate(&mut OsRng);
  let store = IndexedDbStore::open(&name, &key).await.unwrap();
  store.flush().await.unwrap();

  store.begin_writes().unwrap();
  store.put(Table::Users, "half of a group", b"1").unwrap();
  store.write_now();
  assert!(matches!(store.flush().await, Err(StoreError::GroupFailed(_))));

  let other = IndexedDbStore::open_without_upgrade(&name, &key).await.unwrap();
  assert_eq!(other.get(Table::Users, "half of a group").unwrap(), None);
}

/// Issue #1161: a purge deletes the conversation's two key ranges in IndexedDB – every picture of
/// it, however its messages are numbered – and nothing of another conversation or the shared
/// group, whose keys sort right next to it. A download ending after the purge isn't kept.
#[cfg(feature = "service-worker")]
#[wasm_bindgen_test]
async fn a_purge_drops_a_conversation_s_pictures_and_nothing_else() {
  use teamcraft_core::media_cache::{BrowserMediaCache, CachePolicy, CachedMedia, MediaKey};

  let name = database_name();
  let cache = BrowserMediaCache::open(&name).await.unwrap();
  let purged = id::<ConversationId>(1);
  let rendition = |conversation: ConversationId, message: i64| MediaKey::Rendition {
    conversation,
    message: id(message),
    attachment: id(message),
    content_sha256: [1; 32],
  };
  let picture = || CachedMedia {
    version: "v".to_owned(),
    content_type: "image/png".to_owned(),
    bytes: b"a picture".to_vec().into(),
  };
  let gone = [
    rendition(purged, 1),
    rendition(purged, i64::MAX),
    MediaKey::ChannelImage(purged),
  ];
  let kept = [
    rendition(id(2), 1),
    MediaKey::ChannelImage(id(2)),
    MediaKey::Avatar(id(1)),
    MediaKey::SpaceImage(id(1)),
  ];
  for key in gone.iter().chain(&kept) {
    cache.put(key, &picture(), CachePolicy::default()).await.unwrap();
  }

  cache.remove_conversation(purged).await.unwrap();
  cache
    .put(&rendition(purged, 5), &picture(), CachePolicy::default())
    .await
    .unwrap();

  let max_age = Duration::from_secs(3600);
  for key in gone.iter().chain([&rendition(purged, 5)]) {
    assert!(cache.get(key, max_age).await.is_none(), "{key:?}");
  }
  for key in &kept {
    assert!(cache.get(key, max_age).await.is_some(), "{key:?}");
  }
  cache.close();
  drop(cache);
  let reopened = BrowserMediaCache::open(&name).await.unwrap();
  assert!(
    reopened
      .get(&MediaKey::ChannelImage(purged), max_age)
      .await
      .is_none()
  );
  assert!(reopened.get(&kept[0], max_age).await.is_some());
  reopened.remove_conversation(purged).await.unwrap();
}

fn id<T: Id>(raw: i64) -> T {
  T::from_snowflake(Snowflake::new(raw).unwrap())
}

/// Aborts the transaction of the next `IDBObjectStore.put` once the page returned to the browser, when
/// its batch has been issued whole: what a write refused late does (quota, on commit).
fn abort_next_write_transaction() {
  js_sys::eval(
    "const put = IDBObjectStore.prototype.put; IDBObjectStore.prototype.put = function (...args) { \
     IDBObjectStore.prototype.put = put; const transaction = this.transaction; \
     queueMicrotask(() => transaction.abort()); return put.apply(this, args); };",
  )
  .unwrap();
}

/// Makes `IDBObjectStore.put` throw `QuotaExceededError`, as a full disk does, or restores it.
fn refuse_puts(is_refusing: bool) {
  let script = if is_refusing {
    "globalThis.__put = IDBObjectStore.prototype.put; IDBObjectStore.prototype.put = function () { \
     throw new DOMException('quota', 'QuotaExceededError'); };"
  } else {
    "IDBObjectStore.prototype.put = globalThis.__put;"
  };
  js_sys::eval(script).unwrap();
}

/// Deletes the database `name`.
async fn delete_database(name: &str) {
  use wasm_bindgen::JsCast;
  let factory: web_sys::IdbFactory = js_sys::Reflect::get(&js_sys::global(), &"indexedDB".into())
    .unwrap()
    .unchecked_into();
  let request = factory.delete_database(name).unwrap();
  let deleted = js_sys::Promise::new(&mut |resolve, reject| {
    request.set_onsuccess(Some(&resolve));
    request.set_onerror(Some(&reject));
  });
  wasm_bindgen_futures::JsFuture::from(deleted).await.unwrap();
}

/// Opens `name` with a higher schema version, as a newer release of the app would.
async fn open_with_newer_version(name: &str) {
  use wasm_bindgen::JsCast;
  let factory: web_sys::IdbFactory = js_sys::Reflect::get(&js_sys::global(), &"indexedDB".into())
    .unwrap()
    .unchecked_into();
  let request = factory.open_with_u32(name, 1_000).unwrap();
  let opened = js_sys::Promise::new(&mut |resolve, reject| {
    request.set_onsuccess(Some(&resolve));
    request.set_onerror(Some(&reject));
  });
  let database: web_sys::IdbDatabase = wasm_bindgen_futures::JsFuture::from(opened)
    .await
    .map(|_| request.result().unwrap())
    .unwrap()
    .unchecked_into();
  database.close();
}

/// Deletes the record saying how far the catalog is built (sealed.rs `entry_id`), so the next open
/// builds it: what an update that changes what the catalog holds does.
async fn forget_catalog_state(name: &str, key: &SymmetricKey) {
  use wasm_bindgen::JsCast;
  let id = LocalStoreKeys::derive(key).lookup_id("entry", b"_catalog\0meta");
  let record: String = id.0.iter().map(|byte| format!("{byte:02x}")).collect();
  let script = format!(
    "new Promise((resolve, reject) => {{
      const request = indexedDB.open({name:?});
      request.onerror = () => reject(request.error);
      request.onsuccess = () => {{
        const transaction = request.result.transaction('records', 'readwrite');
        transaction.objectStore('records').delete({record:?});
        transaction.oncomplete = () => {{ request.result.close(); resolve(); }};
        transaction.onerror = () => reject(transaction.error);
      }};
    }})"
  );
  let deleted: js_sys::Promise = js_sys::eval(&script).unwrap().unchecked_into();
  wasm_bindgen_futures::JsFuture::from(deleted).await.unwrap();
}

/// A schema 2 database as a version from 1.0.9 up to the catalog left it: no catalog, the MLS
/// state per entry, `USERS` entries and `MESSAGES` messages. Returns its key and its version.
async fn write_schema_2_database(name: &str) -> (SymmetricKey, f64) {
  create_version(name, 2).await;
  let key = browser_database_key_without_upgrade(name).await.unwrap();
  let store = IndexedDbStore::open_without_upgrade(name, &key).await.unwrap();
  store.begin_writes().unwrap();
  for index in 0..USERS {
    store
      .put(
        Table::Users,
        &format!("user {index}"),
        format!("value {index}").as_bytes(),
      )
      .unwrap();
  }
  for client in 0..MESSAGES {
    store
      .upsert_message(&message(1, client, &format!("message {client}")))
      .unwrap();
  }
  store.end_writes().unwrap();
  store.flush().await.unwrap();
  drop(store);
  let version = database_version(name).await;
  assert_eq!(version, 2.0, "a store opened as it is upgrades nothing");
  (key, version)
}

/// Everything `write_schema_2_database` wrote, listed and read by key.
fn assert_everything_is_there(store: &IndexedDbStore) {
  let users = store.list(Table::Users).unwrap();
  assert_eq!(
    users.len(),
    USERS.max(users.len().min(USERS + 1)).min(users.len())
  );
  for index in 0..USERS {
    assert_eq!(
      store.get(Table::Users, &format!("user {index}")).unwrap(),
      Some(format!("value {index}").into_bytes())
    );
  }
  let wanted: Vec<String> = (0..MESSAGES).map(|client| format!("message {client}")).collect();
  let found: Vec<String> = texts(store, id(1)).into_iter().take(wanted.len()).collect();
  assert_eq!(found, wanted);
}

/// The schema version this build of the app gives a new database.
async fn current_version() -> f64 {
  let name = database_name();
  let key = browser_database_key(&name).await.unwrap();
  drop(IndexedDbStore::open(&name, &key).await.unwrap());
  database_version(&name).await
}

/// Creates `name` at an early schema `version` (the object stores are those of version 1).
async fn create_version(name: &str, version: u32) {
  use wasm_bindgen::JsCast;
  let script = format!(
    "new Promise((resolve, reject) => {{
      const request = indexedDB.open({name:?}, {version});
      request.onupgradeneeded = () => {{
        request.result.createObjectStore('records');
        request.result.createObjectStore('keys');
      }};
      request.onsuccess = () => {{ request.result.close(); resolve(); }};
      request.onerror = () => reject(request.error);
    }})"
  );
  let created: js_sys::Promise = js_sys::eval(&script).unwrap().unchecked_into();
  wasm_bindgen_futures::JsFuture::from(created).await.unwrap();
}

/// The version `name` has now (opened without one, so nothing is upgraded).
async fn database_version(name: &str) -> f64 {
  use wasm_bindgen::JsCast;
  let script = format!(
    "new Promise((resolve, reject) => {{
      const request = indexedDB.open({name:?});
      request.onsuccess = () => {{
        const version = request.result.version;
        request.result.close();
        resolve(version);
      }};
      request.onerror = () => reject(request.error);
    }})"
  );
  let opened: js_sys::Promise = js_sys::eval(&script).unwrap().unchecked_into();
  wasm_bindgen_futures::JsFuture::from(opened)
    .await
    .unwrap()
    .as_f64()
    .unwrap()
}

/// Each test gets its own database, so tests don't see each other's data.
fn database_name() -> String {
  format!("teamcraft-test-{}", OsRng.next_u64())
}

/// The browser's picture codec (`thumbnail/browser.rs`, #135) keeps the native one's contract:
/// size, format by transparency, the pixel limit read from the header, junk.
mod pictures {
  use teamcraft_core::image_header::{media_type, upright_size};
  use teamcraft_core::thumbnail::browser::{Encoding, canvas, encode};
  use teamcraft_core::thumbnail::{BrowserCodec, MAX_THUMBNAIL_BYTES, PictureCodec, THUMBNAIL_SIZE};
  use wasm_bindgen_test::wasm_bindgen_test;

  /// A `width` × `height` PNG drawn by the browser: red, its left half transparent when asked.
  async fn png(width: u32, height: u32, is_half_transparent: bool) -> Vec<u8> {
    let (canvas, context) = canvas(width, height).unwrap();
    context.set_fill_style_str("#c00");
    let left = if is_half_transparent {
      f64::from(width) / 2.0
    } else {
      0.0
    };
    context.fill_rect(left, 0.0, f64::from(width) - left, f64::from(height));
    encode(&canvas, Encoding::Png, usize::MAX).await.unwrap()
  }

  #[wasm_bindgen_test]
  async fn an_opaque_picture_becomes_a_jpeg_of_the_thumbnail_size() {
    let picture = png(1000, 500, false).await;
    let made = BrowserCodec.thumbnail(&picture, "image/png").await.unwrap();
    assert_eq!(media_type(&made), Some("image/jpeg"));
    assert_eq!(upright_size(&made), Some((THUMBNAIL_SIZE, THUMBNAIL_SIZE / 2)));
    assert!(made.len() <= MAX_THUMBNAIL_BYTES);
    // The engine's entry point is the same codec in the browser.
    let through_engine = teamcraft_core::thumbnail::make_thumbnail(&picture, "Image/PNG; x=y").await;
    assert_eq!(
      through_engine.as_deref().map(media_type),
      Some(Some("image/jpeg"))
    );
  }

  #[wasm_bindgen_test]
  async fn a_transparent_picture_stays_a_png_and_small_ones_keep_their_size() {
    let picture = png(200, 100, true).await;
    let made = BrowserCodec.thumbnail(&picture, "image/png").await.unwrap();
    assert_eq!(media_type(&made), Some("image/png"));
    assert_eq!(upright_size(&made), Some((200, 100)));
  }

  #[wasm_bindgen_test]
  async fn junk_and_pictures_over_the_pixel_limit_have_no_thumbnail() {
    assert_eq!(
      BrowserCodec.thumbnail(b"not really a png", "image/png").await,
      None
    );
    assert_eq!(BrowserCodec.thumbnail(b"", "image/png").await, None);
    let picture = png(10, 10, false).await;
    assert_eq!(BrowserCodec.thumbnail(&picture, "application/pdf").await, None);
    // The header says 20000 × 20000: refused before the browser decodes it.
    let mut bomb = picture.clone();
    bomb[16..20].copy_from_slice(&20_000u32.to_be_bytes());
    bomb[20..24].copy_from_slice(&20_000u32.to_be_bytes());
    let crc = crc32(&bomb[12..29]);
    bomb[29..33].copy_from_slice(&crc.to_be_bytes());
    assert_eq!(upright_size(&bomb), Some((20_000, 20_000)));
    assert_eq!(BrowserCodec.thumbnail(&bomb, "image/png").await, None);
  }

  #[wasm_bindgen_test]
  async fn a_screenshot_is_encoded_again_at_its_size() {
    let picture = png(64, 32, false).await;
    let (clean, made_type) = BrowserCodec.without_metadata(&picture).await.unwrap();
    assert_eq!((made_type, upright_size(&clean)), ("image/png", Some((64, 32))));
    assert_eq!(BrowserCodec.without_metadata(b"%PDF-1.7").await, None);
  }

  fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
      crc ^= u32::from(*byte);
      for _ in 0..8 {
        crc = if crc & 1 == 1 {
          (crc >> 1) ^ 0xEDB8_8320
        } else {
          crc >> 1
        };
      }
    }
    !crc
  }
}
