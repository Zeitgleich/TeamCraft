//! Databases written by released versions (`tests/fixtures/store/`), opened by every later version
//! (issue #160): a schema change that can't read them fails here, not on a user's device.
//!
//! Each release adds its pair of files with `generate_fixtures` (ignored; run it on the release
//! commit): `cargo test -p teamcraft-core --all-features -- --ignored generate_fixtures`.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider;
use rand_core::OsRng;
use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair};
use teamcraft_crypto::key_bag::KeyBag;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{ConversationId, Id, MessageId, MlsMessageId, Snowflake, UserId};

use super::{LocalStore, MessageContent, MessageFilter, SealedSqliteStore, SqlCipherStore, StaticKeySource};
use crate::convert;
use crate::mls_storage;
use crate::records::{self, Account, Draft, SyncState};

/// The key every fixture database is encrypted with.
const FIXTURE_KEY: [u8; 32] = [0x5a; 32];
const USER: i64 = 5;
const DEVICE: i64 = 6;
const CONVERSATION: i64 = 70;
const SENT_MESSAGE: i64 = 900;
const MLS_CURSOR: i64 = 800;
const SENT_BODY: &str = "a message the server confirmed";
const PENDING_BODY: &str = "a message waiting in the outbox";

#[test]
fn every_released_database_opens() {
  let fixtures = fixture_files();
  assert!(
    fixtures
      .iter()
      .any(|path| path.to_string_lossy().contains("1.0.2")),
    "the fixtures of the first released schema are missing: {fixtures:?}"
  );
  for path in fixtures {
    // Opening migrates in place: work on a copy.
    let copy = scratch_dir().join(path.file_name().unwrap());
    std::fs::copy(&path, &copy).unwrap();
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    if name.ends_with("-sqlcipher.db") {
      check(&SqlCipherStore::open(&copy, &key()).unwrap(), &name);
    } else if name.ends_with("-sealed.db") {
      check(&SealedSqliteStore::open(&copy, &key()).unwrap(), &name);
    } else {
      panic!("unknown fixture {name}");
    }
  }
}

#[test]
#[ignore = "writes the fixtures of this version; run on the release commit"]
fn generate_fixtures() {
  let directory = fixture_dir();
  std::fs::create_dir_all(&directory).unwrap();
  let version = env!("CARGO_PKG_VERSION");
  for (suffix, is_sealed) in [("sqlcipher", false), ("sealed", true)] {
    let path = directory.join(format!("v{version}-{suffix}.db"));
    let _ = std::fs::remove_file(&path);
    if is_sealed {
      fill(&SealedSqliteStore::open(&path, &key()).unwrap());
    } else {
      fill(&SqlCipherStore::open(&path, &key()).unwrap());
    }
    // A sealed store keeps a write-ahead log until it is closed; the fixture is the main file only.
    for extension in ["db-wal", "db-shm"] {
      let _ = std::fs::remove_file(path.with_extension(extension));
    }
  }
}

/// What a fixture holds: an account, sync cursors, an MLS snapshot, a sent and a pending message.
fn fill(store: &impl LocalStore) {
  let user_id: UserId = id(USER);
  let identity = IdentityKeyPair::generate(&mut OsRng);
  let device = DeviceKeyPair::generate(convert::user(user_id), convert::device(id(DEVICE)), &mut OsRng);
  let certificate = identity
    .certify_device(convert::user(user_id), &device.public_keys())
    .unwrap();
  let account = Account {
    user_id,
    device_id: id(DEVICE),
    device,
    certificate,
    backup_secret: SymmetricKey::generate(&mut OsRng),
    key_bag: KeyBag::new(convert::user(user_id), identity),
  };
  records::save_account(store, &account).unwrap();
  let conversation: ConversationId = id(CONVERSATION);
  let mut sync = SyncState::default();
  sync
    .mls_cursors
    .insert(conversation, id::<MlsMessageId>(MLS_CURSOR));
  records::save_sync_state(store, &sync).unwrap();
  let provider = OpenMlsRustCrypto::default();
  provider
    .storage()
    .values
    .write()
    .unwrap()
    .insert(b"group".to_vec(), b"state".to_vec());
  mls_storage::save_mls_storage(store, &provider, &mls_storage::SavedMlsStorage::default()).unwrap();
  let sent = records::enqueue_message(
    store,
    user_id,
    1_000,
    Draft {
      conversation_id: conversation,
      maybe_thread_id: None,
      content: MessageContent::text(SENT_BODY, Vec::new()),
    },
  )
  .unwrap();
  let mut message = store.message(conversation, sent).unwrap().unwrap();
  message.maybe_server_id = Some(id(SENT_MESSAGE));
  store.upsert_message(&message).unwrap();
  store
    .delete(super::Table::Outbox, &records::outbox_key(conversation, sent))
    .unwrap();
  records::enqueue_message(
    store,
    user_id,
    2_000,
    Draft {
      conversation_id: conversation,
      maybe_thread_id: None,
      content: MessageContent::text(PENDING_BODY, Vec::new()),
    },
  )
  .unwrap();
}

/// Everything [`fill`] wrote reads back.
fn check(store: &impl LocalStore, name: &str) {
  let account = records::load_account(store)
    .unwrap()
    .unwrap_or_else(|| panic!("{name}: no account"));
  assert_eq!(account.user_id, id(USER), "{name}");
  let conversation: ConversationId = id(CONVERSATION);
  assert_eq!(
    records::load_sync_state(store)
      .unwrap()
      .mls_cursors
      .get(&conversation),
    Some(&id(MLS_CURSOR)),
    "{name}"
  );
  // Opening moves the MLS state of a version before 1.0.9 to entries; opened again, it reads them.
  for _ in 0..2 {
    let (provider, _) = mls_storage::load_mls_storage(store).unwrap();
    assert_eq!(
      provider.storage().values.read().unwrap().get(b"group".as_slice()),
      Some(&b"state".to_vec()),
      "{name}"
    );
  }
  let messages = store.messages(conversation, MessageFilter::TopLevel).unwrap();
  let bodies: Vec<&str> = messages
    .iter()
    .filter_map(|message| match &message.content {
      MessageContent::Text { body, .. } => Some(body.as_str()),
      _ => None,
    })
    .collect();
  assert_eq!(bodies, [SENT_BODY, PENDING_BODY], "{name}");
  assert_eq!(
    store
      .message_by_server_id(conversation, id::<MessageId>(SENT_MESSAGE))
      .unwrap()
      .map(|message| message.client_message_id),
    Some(messages[0].client_message_id),
    "{name}"
  );
  let outbox = records::outbox(store).unwrap();
  assert_eq!(outbox.len(), 1, "{name}");
  assert_eq!(
    outbox[0].client_message_id, messages[1].client_message_id,
    "{name}"
  );
}

fn fixture_files() -> Vec<PathBuf> {
  let mut files: Vec<PathBuf> = std::fs::read_dir(fixture_dir())
    .unwrap()
    .map(|entry| entry.unwrap().path())
    .filter(|path| path.extension().is_some_and(|extension| extension == "db"))
    .collect();
  files.sort();
  files
}

fn fixture_dir() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/store")
}

fn scratch_dir() -> PathBuf {
  let directory = std::env::temp_dir().join(format!(
    "teamcraft-fixtures-{}",
    rand_core::RngCore::next_u64(&mut OsRng)
  ));
  std::fs::create_dir_all(&directory).unwrap();
  directory
}

fn key() -> StaticKeySource {
  StaticKeySource::new(SymmetricKey::from_bytes(FIXTURE_KEY))
}

fn id<T: Id>(raw: i64) -> T {
  T::from_snowflake(Snowflake::new(raw).unwrap())
}
