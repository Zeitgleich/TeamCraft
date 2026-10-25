//! Local store integrity (issues #160, #168): a crash on any write never leaves a stuck message or
//! a group out of step with its cursor, concurrent edits converge, and a restart during linking
//! keeps the keys the existing device handed over.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use crate::common;

use common::{PASSPHRASE, Peer, TestClient, clock, link_device, settings, settle};
use teamcraft_core::api::fake::{FakeGateway, FakeServer, FakeServerApi};
use teamcraft_core::records;
use teamcraft_core::store::{
  LocalStore, MemoryStore, MessageContent, MessageFilter, StoreError, StoredMessage, Table,
};
use teamcraft_core::{Client, LinkOffer, PendingEnrollment};
use teamcraft_proto::{ClientMessageId, ConversationId};

/// Writes a crash-test store takes before it fails; more than any scenario here needs.
const MAX_KILL_POINTS: usize = 400;

mod kill_points {
  use super::*;

  #[tokio::test]
  async fn a_crash_on_any_write_of_a_send_never_leaves_a_stuck_message() {
    for kill_point in 0..MAX_KILL_POINTS {
      let server = FakeServer::new();
      let mut bob = Peer::enroll(&server, "Bob").await;
      let (mut alice, mut alice_gateway, store) = enroll_on_kill_point_store(&server, "Alice").await;
      let channel = alice.create_channel(bob.space().await, "general").await.unwrap();
      alice.add_member(channel, bob.client.user_id()).await.unwrap();
      pump_until_quiet(&mut alice, &mut alice_gateway, &mut bob).await;

      store.crash_after(kill_point);
      let sent = alice.send_text(channel, None, "hello").await;
      let has_crashed = store.has_crashed();
      if !has_crashed {
        sent.unwrap();
      }

      // The app starts again on what reached the disk and sends what is pending.
      let durable = store.durable();
      assert_no_stuck_message(&durable, channel);
      let mut reopened = reopen(&alice, durable.clone());
      reopened.flush_outbox().await.unwrap();
      assert!(
        durable
          .messages(channel, MessageFilter::All)
          .unwrap()
          .iter()
          .all(|message| message.maybe_server_id.is_some()),
        "kill point {kill_point}: a message is still pending after the restart"
      );
      let (mut gateway, _) = reopened.connect_gateway().await.unwrap();
      pump_until_quiet(&mut reopened, &mut gateway, &mut bob).await;
      let copies = bob.texts(channel).iter().filter(|text| *text == "hello").count();
      assert!(copies <= 1, "kill point {kill_point}: sent {copies} times");
      if !has_crashed {
        assert_eq!(copies, 1);
        return;
      }
    }
    panic!("the send never finished within {MAX_KILL_POINTS} writes");
  }

  #[tokio::test]
  async fn a_crash_on_any_write_of_a_group_change_keeps_the_group_in_step() {
    for kill_point in 0..MAX_KILL_POINTS {
      let server = FakeServer::new();
      let mut alice = Peer::enroll(&server, "Alice").await;
      let carol = Peer::enroll(&server, "Carol").await;
      let (mut bob, mut bob_gateway, store) = enroll_on_kill_point_store(&server, "Bob").await;
      let channel = alice
        .client
        .create_channel(alice.space().await, "general")
        .await
        .unwrap();
      alice.client.add_member(channel, bob.user_id()).await.unwrap();
      pump_until_quiet(&mut bob, &mut bob_gateway, &mut alice).await;

      // Bob processes a commit (Carol joins, the key rotates) and a message, and dies on one write.
      store.crash_after(kill_point);
      alice
        .client
        .add_member(channel, carol.client.user_id())
        .await
        .unwrap();
      alice.client.send_text(channel, None, "before").await.unwrap();
      let processed = pump_until_quiet_or_crash(&mut bob, &mut bob_gateway, &mut alice).await;
      let has_crashed = store.has_crashed();
      if !has_crashed {
        processed.unwrap();
      }

      let mut reopened = reopen(&bob, store.durable());
      let (mut gateway, _) = reopened.connect_gateway().await.unwrap();
      pump_until_quiet(&mut reopened, &mut gateway, &mut alice).await;
      alice.client.send_text(channel, None, "after").await.unwrap();
      reopened.send_text(channel, None, "bob is here").await.unwrap();
      pump_until_quiet(&mut reopened, &mut gateway, &mut alice).await;
      let bob_texts = texts(&store.durable(), channel);
      assert!(
        bob_texts.contains(&"after".to_string()),
        "kill point {kill_point}: Bob can't read the group after the restart: {bob_texts:?}"
      );
      assert!(
        alice.texts(channel).contains(&"bob is here".to_string()),
        "kill point {kill_point}: Alice can't read Bob after his restart"
      );
      if !has_crashed {
        return;
      }
    }
    panic!("processing never finished within {MAX_KILL_POINTS} writes");
  }
}

mod linking {
  use super::*;

  #[tokio::test]
  async fn a_restart_after_the_transfer_finishes_enrolling_without_a_new_link() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let laptop_store = Arc::new(MemoryStore::new());
    receive_link(&mut alice, &laptop_store).await;
    // The laptop stops here: the phone reported success, the laptop has no account yet.
    assert!(
      Client::open(alice.api.clone(), laptop_store.clone(), clock(), settings())
        .unwrap()
        .is_none()
    );

    let laptop = PendingEnrollment::resume_link(
      alice.api.clone(),
      laptop_store.clone(),
      clock(),
      settings(),
      common::options(PASSPHRASE).argon2,
    )
    .await
    .unwrap()
    .expect("the stored keys finish the enrollment");
    assert_eq!(laptop.identity_fingerprint(), alice.client.identity_fingerprint());
    assert_ne!(laptop.device_id(), alice.client.device_id());
    // Done once: the keys are gone from the pending record, the account is stored.
    assert!(records::load_pending_link(&laptop_store).unwrap().is_none());
    assert!(
      Client::open(alice.api.clone(), laptop_store, clock(), settings())
        .unwrap()
        .is_some()
    );
  }

  #[tokio::test]
  async fn resuming_offline_keeps_the_keys_for_the_next_start() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let laptop_store = Arc::new(MemoryStore::new());
    receive_link(&mut alice, &laptop_store).await;

    server.set_offline(alice.client.user_id(), true);
    let offline = PendingEnrollment::resume_link(
      alice.api.clone(),
      laptop_store.clone(),
      clock(),
      settings(),
      common::options(PASSPHRASE).argon2,
    )
    .await;
    assert!(offline.unwrap_err().is_transient());
    assert!(records::load_pending_link(&laptop_store).unwrap().is_some());

    server.set_offline(alice.client.user_id(), false);
    let resumed = PendingEnrollment::resume_link(
      alice.api.clone(),
      laptop_store,
      clock(),
      settings(),
      common::options(PASSPHRASE).argon2,
    )
    .await
    .unwrap();
    assert!(resumed.is_some());
  }

  #[tokio::test]
  async fn keys_of_a_link_for_another_account_are_dropped() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let bob = Peer::enroll(&server, "Bob").await;
    let store = Arc::new(MemoryStore::new());
    receive_link(&mut alice, &store).await;
    // Signed in as somebody else on this device since.
    let resumed = PendingEnrollment::resume_link(
      bob.api.clone(),
      store.clone(),
      clock(),
      settings(),
      common::options(PASSPHRASE).argon2,
    )
    .await
    .unwrap();
    assert!(resumed.is_none());
    assert!(records::load_pending_link(&store).unwrap().is_none());
  }

  /// Links a new device of `owner` whose secrets land in `store`, and stops before it enrolls.
  async fn receive_link(owner: &mut Peer, store: &Arc<MemoryStore>) {
    let offer = LinkOffer::generate();
    let code = offer.to_code();
    let (new_side, owner_side) = tokio::join!(
      teamcraft_core::client::NewDeviceLink::connect(&owner.api, &offer),
      owner.client.start_link_approval(&code)
    );
    let (approved, received) = tokio::join!(
      owner.client.complete_link_approval(owner_side.unwrap()),
      new_side.unwrap().receive_secrets(store, "Laptop")
    );
    approved.unwrap();
    drop(received.unwrap());
    let pending = records::load_pending_link(store).unwrap().unwrap();
    assert_eq!(pending.device_name, "Laptop");
  }
}

#[tokio::test]
async fn concurrent_edits_from_two_devices_converge() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = phone
    .client
    .create_channel(phone.space().await, "general")
    .await
    .unwrap();
  phone
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob]).await;
  let original = phone
    .client
    .send_text(channel, None, "Meeting at 10")
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob]).await;
  phone.client.upload_key_bag().await.unwrap();
  let mut laptop = linked_peer(&mut phone).await;
  laptop.client.full_sync().await.unwrap();
  settle(&mut [&mut phone, &mut laptop, &mut bob]).await;

  // Both devices edit before either hears of the other's edit; the laptop's reaches the server last.
  phone
    .client
    .edit_message(channel, original, "Meeting at 11")
    .await
    .unwrap();
  laptop
    .client
    .edit_message(channel, original, "Meeting at 12")
    .await
    .unwrap();
  assert!(
    records::outbox(&laptop.store).unwrap().is_empty(),
    "the laptop's edit went out"
  );
  settle(&mut [&mut phone, &mut laptop, &mut bob]).await;

  for peer in [&phone, &laptop, &bob] {
    let stored = peer.store.message(channel, original).unwrap().unwrap();
    assert!(
      matches!(&stored.content, MessageContent::Text { body, .. } if body == "Meeting at 12"),
      "{} shows {:?}",
      peer.name,
      stored.content
    );
  }
}

/// A store that crashes: armed with [`Self::crash_after`], it takes that many writes, then fails
/// every write as a dead process would. [`Self::durable`] is what reached the disk: writes outside
/// a group, and groups that ended before the crash.
#[derive(Default)]
struct KillPointStore {
  live: MemoryStore,
  durable: Arc<MemoryStore>,
  state: Mutex<KillState>,
}

#[derive(Default)]
struct KillState {
  maybe_writes_left: Option<usize>,
  has_crashed: bool,
  depth: u32,
  group: Vec<Write>,
}

#[derive(Clone)]
enum Write {
  Put(Table, String, Vec<u8>),
  Delete(Table, String),
  Upsert(Box<StoredMessage>),
  DeleteMessages(ConversationId),
  DeleteMessage(ConversationId, ClientMessageId),
}

impl KillPointStore {
  fn crash_after(&self, writes: usize) {
    self.state.lock().unwrap().maybe_writes_left = Some(writes);
  }

  fn has_crashed(&self) -> bool {
    self.state.lock().unwrap().has_crashed
  }

  fn durable(&self) -> Arc<MemoryStore> {
    self.durable.clone()
  }

  fn write(&self, write: Write) -> Result<(), StoreError> {
    let mut state = self.state.lock().unwrap();
    if state.has_crashed {
      return Err(crashed());
    }
    if let Some(left) = state.maybe_writes_left.as_mut() {
      if *left == 0 {
        state.has_crashed = true;
        return Err(crashed());
      }
      *left -= 1;
    }
    apply(&self.live, &write)?;
    if state.depth > 0 {
      state.group.push(write);
      Ok(())
    } else {
      apply(&self.durable, &write)
    }
  }
}

impl LocalStore for KillPointStore {
  fn get(&self, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    self.live.get(table, key)
  }

  fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError> {
    self.write(Write::Put(table, key.to_string(), value.to_vec()))
  }

  fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
    self.write(Write::Delete(table, key.to_string()))
  }

  fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    self.live.list(table)
  }

  fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
    self.write(Write::Upsert(Box::new(message.clone())))
  }

  fn message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    self.live.message(conversation, client_message_id)
  }

  fn messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.live.messages(conversation, filter)
  }

  fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
    self.write(Write::DeleteMessages(conversation))
  }

  fn delete_message(&self, conversation: ConversationId, message: ClientMessageId) -> Result<(), StoreError> {
    self.write(Write::DeleteMessage(conversation, message))
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    let mut state = self.state.lock().unwrap();
    if state.has_crashed {
      return Err(crashed());
    }
    state.depth += 1;
    Ok(())
  }

  fn end_writes(&self) -> Result<(), StoreError> {
    let mut state = self.state.lock().unwrap();
    state.depth = state.depth.saturating_sub(1);
    if state.depth > 0 {
      return Ok(());
    }
    let group = std::mem::take(&mut state.group);
    if state.has_crashed {
      return Err(crashed());
    }
    for write in &group {
      apply(&self.durable, write)?;
    }
    Ok(())
  }
}

fn apply(store: &MemoryStore, write: &Write) -> Result<(), StoreError> {
  match write {
    Write::Put(table, key, value) => store.put(*table, key, value),
    Write::Delete(table, key) => store.delete(*table, key),
    Write::Upsert(message) => store.upsert_message(message),
    Write::DeleteMessages(conversation) => store.delete_messages(*conversation),
    Write::DeleteMessage(conversation, message) => store.delete_message(*conversation, *message),
  }
}

fn crashed() -> StoreError {
  StoreError::Database("the app crashed".into())
}

type KillPointClient = Client<FakeServerApi, Arc<KillPointStore>>;

async fn enroll_on_kill_point_store(
  server: &FakeServer,
  name: &str,
) -> (KillPointClient, FakeGateway, Arc<KillPointStore>) {
  let api = server.add_user(name);
  let store = Arc::new(KillPointStore::default());
  let mut client = PendingEnrollment::new()
    .complete(
      api,
      store.clone(),
      clock(),
      settings(),
      common::options(PASSPHRASE),
    )
    .await
    .unwrap();
  let (gateway, _) = client.connect_gateway().await.unwrap();
  (client, gateway, store)
}

/// The client of `client`'s account on `durable`, as the app opens it after a crash.
fn reopen<S: LocalStore>(client: &Client<FakeServerApi, S>, durable: Arc<MemoryStore>) -> TestClient {
  Client::open(client.api().clone(), durable, clock(), settings())
    .unwrap()
    .expect("the account survived")
}

/// Every own message without a server ID is in the outbox: nothing shows "sending" forever.
fn assert_no_stuck_message(store: &MemoryStore, conversation: ConversationId) {
  for message in store.messages(conversation, MessageFilter::All).unwrap() {
    if message.maybe_server_id.is_none() {
      let key = records::outbox_key(conversation, message.client_message_id);
      assert!(
        store.get(Table::Outbox, &key).unwrap().is_some(),
        "a pending message without its outbox entry"
      );
    }
  }
}

async fn pump_until_quiet<S: LocalStore>(
  client: &mut Client<FakeServerApi, S>,
  gateway: &mut FakeGateway,
  other: &mut Peer,
) {
  pump_until_quiet_or_crash(client, gateway, other).await.unwrap();
}

async fn pump_until_quiet_or_crash<S: LocalStore>(
  client: &mut Client<FakeServerApi, S>,
  gateway: &mut FakeGateway,
  other: &mut Peer,
) -> Result<(), teamcraft_core::CoreError> {
  for _ in 0..50 {
    let handled = client.pump(gateway).await? + other.client.pump(&mut other.gateway).await.unwrap();
    if handled == 0 {
      return Ok(());
    }
  }
  panic!("clients did not settle");
}

/// A second device of `owner`'s account, connected.
async fn linked_peer(owner: &mut Peer) -> Peer {
  let mut client = link_device(owner).await;
  let (gateway, _) = client.connect_gateway().await.unwrap();
  Peer {
    name: format!("{} (laptop)", owner.name),
    api: owner.api.clone(),
    store: client.store().clone(),
    client,
    gateway,
  }
}

fn texts(store: &MemoryStore, conversation: ConversationId) -> Vec<String> {
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
