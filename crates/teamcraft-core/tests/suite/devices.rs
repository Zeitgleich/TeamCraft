//! A colleague's new device (`device_add`) and the read state the server reports (threads,
//! receipts, acknowledgements), through the fake server.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use common::{PASSPHRASE, Peer, linked_options, options, settings, settle};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::{FakeServer, FakeServerApi};
use teamcraft_core::client::NewDeviceLink;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::read_state;
use teamcraft_core::store::{LocalStore, MemoryStore, Table};
use teamcraft_core::{Client, ClientSettings, CoreError, LinkOffer, PendingEnrollment};
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::{
  Base64Bytes, ConversationId, GatewayEvent, ListMlsMessagesQuery, RegisterDeviceRequest,
};

const STAGGER: Duration = Duration::from_secs(10);

mod device_add {
  use super::*;

  #[tokio::test]
  async fn one_member_adds_a_new_device_and_the_others_only_check() {
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let server = FakeServer::new();
    let mut alice = enroll(&server, "Alice", &clock).await;
    let mut bob = enroll(&server, "Bob", &clock).await;
    let mut carol = enroll(&server, "Carol", &clock).await;
    let channel = channel_of(&mut [&mut alice, &mut bob, &mut carol]).await;

    let laptop = link_new_device(&mut bob, &clock).await;
    // Everybody hears `device_add`; the member ranked first commits right away.
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert_eq!(welcomes_for(&laptop).await, 1);

    // Later ranks come due, find the device in the group and drop their entries without a commit.
    clock.advance(millis(STAGGER) * 3);
    for peer in [&mut alice, &mut bob, &mut carol] {
      peer.client.apply_pending_changes().await.unwrap();
      assert!(peer.store.list(Table::PendingChanges).unwrap().is_empty());
    }
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert_eq!(welcomes_for(&laptop).await, 1);
    for peer in [&mut alice, &mut bob, &mut carol] {
      assert!(peer.client.has_group(channel).unwrap());
    }
  }

  #[tokio::test]
  async fn a_device_without_key_packages_is_retried_then_given_up() {
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let server = FakeServer::new();
    let mut alice = enroll(&server, "Alice", &clock).await;
    let mut bob = enroll(&server, "Bob", &clock).await;
    channel_of(&mut [&mut alice, &mut bob]).await;

    // A device that registers but never uploads key packages (crashed during setup).
    bob
      .api
      .register_device(&RegisterDeviceRequest {
        name: "Broken".into(),
        public_key: Base64Bytes::new(vec![1; 32]),
        cross_signature: Base64Bytes::new(vec![2; 64]),
      })
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    assert!(!alice.store.list(Table::PendingChanges).unwrap().is_empty());

    for _ in 0..settings_with_stagger().device_add_attempts + 2 {
      clock.advance(millis(STAGGER) * 2);
      alice.client.apply_pending_changes().await.unwrap();
      bob.client.apply_pending_changes().await.unwrap();
    }
    assert!(alice.store.list(Table::PendingChanges).unwrap().is_empty());
    assert!(bob.store.list(Table::PendingChanges).unwrap().is_empty());
  }
}

mod read_state_events {
  use super::*;

  #[tokio::test]
  async fn thread_updates_store_the_followed_threads_with_decrypted_replies() {
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let server = FakeServer::new();
    let mut alice = enroll(&server, "Alice", &clock).await;
    let mut bob = enroll(&server, "Bob", &clock).await;
    let channel = channel_of(&mut [&mut alice, &mut bob]).await;

    alice.client.send_text(channel, None, "root").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let root = server_id_of_last(&alice, channel);
    bob
      .client
      .send_text(channel, Some(teamcraft_proto::Id::from_snowflake(root)), "reply")
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    let threads = read_state::threads(alice.store.as_ref()).unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].reply_count, 1);
    let reply = threads[0].maybe_last_reply.as_ref().unwrap();
    let decrypted = alice
      .store
      .message(channel, reply.client_message_id)
      .unwrap()
      .unwrap();
    assert!(matches!(
      decrypted.content,
      teamcraft_core::store::MessageContent::Text { ref body, .. } if body == "reply"
    ));
    assert!(read_state::unread(alice.store.as_ref()).unwrap().is_some());
  }

  #[tokio::test]
  async fn receipts_and_acknowledgements_are_stored() {
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let server = FakeServer::new();
    let mut alice = enroll(&server, "Alice", &clock).await;
    let mut bob = enroll(&server, "Bob", &clock).await;
    let channel = channel_of(&mut [&mut alice, &mut bob]).await;
    alice.client.send_text(channel, None, "urgent").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let message = teamcraft_proto::Id::from_snowflake(server_id_of_last(&alice, channel));
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());

    server.publish_event(
      &[alice_id],
      &GatewayEvent::ReadReceipt {
        conversation_id: channel,
        user_id: bob_id,
        message_id: message,
      },
    );
    server.publish_event(
      &[alice_id],
      &GatewayEvent::MessageAck {
        conversation_id: channel,
        message_id: message,
        user_id: bob_id,
      },
    );
    settle(&mut [&mut alice]).await;

    assert_eq!(
      read_state::read_receipts(alice.store.as_ref(), channel)
        .unwrap()
        .get(&bob_id),
      Some(&message)
    );
    assert_eq!(
      read_state::acknowledgements(alice.store.as_ref(), channel, message).unwrap(),
      vec![bob_id]
    );
  }
}

async fn enroll(server: &FakeServer, name: &str, clock: &Arc<ManualClock>) -> Peer {
  let api = server.add_user(name);
  let store = Arc::new(MemoryStore::new());
  let mut client = PendingEnrollment::new()
    .complete(
      api.clone(),
      store.clone(),
      Box::new(clock.clone()),
      settings_with_stagger(),
      options(PASSPHRASE),
    )
    .await
    .unwrap();
  let (gateway, _) = client.connect_gateway().await.unwrap();
  Peer {
    name: name.to_string(),
    api,
    store,
    client,
    gateway,
  }
}

/// A channel created by the first peer with all of them as members.
async fn channel_of(peers: &mut [&mut Peer]) -> ConversationId {
  let space = peers[0].space().await;
  let channel = peers[0].client.create_channel(space, "devices").await.unwrap();
  for index in 1..peers.len() {
    let user = peers[index].client.user_id();
    peers[0].client.add_member(channel, user).await.unwrap();
  }
  settle(peers).await;
  channel
}

/// Links a second device of `owner`, as in tests/linking.rs.
async fn link_new_device(
  owner: &mut Peer,
  clock: &Arc<ManualClock>,
) -> Client<FakeServerApi, Arc<MemoryStore>> {
  let api = owner.api.clone();
  let offer = LinkOffer::generate();
  let code = offer.to_code();
  let (new_side, owner_side) = tokio::join!(
    NewDeviceLink::connect(&api, &offer),
    owner.client.start_link_approval(&code)
  );
  let (approved, received) = tokio::join!(
    owner.client.complete_link_approval(owner_side.unwrap()),
    new_side.unwrap().receive_secrets()
  );
  approved.unwrap();
  let link_options = linked_options(received.unwrap());
  PendingEnrollment::new()
    .complete(
      api,
      Arc::new(MemoryStore::new()),
      Box::new(clock.clone()),
      settings_with_stagger(),
      link_options,
    )
    .await
    .unwrap()
}

async fn welcomes_for(client: &Client<FakeServerApi, Arc<MemoryStore>>) -> usize {
  client
    .api()
    .list_welcomes(client.device_id(), &ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .messages
    .len()
}

fn server_id_of_last(peer: &Peer, channel: ConversationId) -> teamcraft_proto::Snowflake {
  use teamcraft_proto::Id;
  peer
    .store
    .messages(channel, teamcraft_core::store::MessageFilter::TopLevel)
    .unwrap()
    .last()
    .and_then(|message| message.maybe_server_id)
    .unwrap()
    .snowflake()
}

fn settings_with_stagger() -> ClientSettings {
  ClientSettings {
    device_add_stagger: STAGGER,
    ..settings()
  }
}

fn millis(duration: Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap()
}

#[tokio::test]
async fn key_bag_rollback_and_swapped_wraps_are_refused() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let me = alice.client.user_id();
  let old = server.key_bag_of(me).unwrap();
  alice
    .client
    .upload_key_bag_with(
      &SecretBytes::new(b"the new passphrase".to_vec()),
      options(PASSPHRASE).argon2,
    )
    .await
    .unwrap();
  let newest = server.key_bag_of(me).unwrap();

  // The server serves the first upload again, under a newer version number (security review B11).
  server.serve_key_bag(me, old.clone());
  assert!(matches!(
    alice.client.sync_account_data().await,
    Err(CoreError::KeyBagRollback { .. })
  ));

  // The server puts the old wraps (still opening with the old passphrase) next to the newest bag:
  // the bag is bound to the wraps it was uploaded with and doesn't open.
  let mut swapped = newest.clone();
  swapped.wrapped_backup_keys = old.wrapped_backup_keys;
  server.serve_key_bag(me, swapped);
  assert!(enroll_again(&alice, PASSPHRASE).await.is_err());
  server.serve_key_bag(me, newest);
  assert!(enroll_again(&alice, "the new passphrase").await.is_ok());
}

async fn enroll_again(peer: &Peer, passphrase: &str) -> Result<common::TestClient, CoreError> {
  PendingEnrollment::new()
    .complete(
      peer.api.clone(),
      Arc::new(MemoryStore::new()),
      common::clock(),
      settings(),
      options(passphrase),
    )
    .await
}

#[tokio::test]
async fn a_linked_device_inherits_pins_and_the_workspace_root() {
  // Security review A2: the new laptop doesn't trust the server on first use again.
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let (alice_id, bob_id) = (phone.client.user_id(), bob.client.user_id());
  phone
    .client
    .publish_roster(&[(alice_id, teamcraft_core::workspace::RosterRole::Owner)])
    .await
    .unwrap();
  let channel = phone
    .client
    .create_channel(phone.space().await, "pins")
    .await
    .unwrap();
  phone.client.add_member(channel, bob_id).await.unwrap();
  settle(&mut [&mut phone, &mut bob]).await;
  phone.client.sync_account_data().await.unwrap();

  let mut laptop = common::link_device(&mut phone).await;
  server.replace_identity_key(bob_id, vec![7; 32]);
  phone.client.add_missing_devices(channel).await.unwrap();
  laptop.sync_welcomes().await.unwrap();
  laptop.add_missing_devices(channel).await.unwrap();

  // The laptop verified the group's members with Bob's key from the phone's pins, not the
  // server's substitute (with that, Bob's credential wouldn't verify and the welcome would fail).
  assert!(laptop.has_group(channel).unwrap());
  laptop.sync_workspace().await.unwrap();
  assert_eq!(
    laptop.workspace_root_fingerprint().unwrap(),
    phone.client.workspace_root_fingerprint().unwrap()
  );
}
