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
  Base64Bytes, ConversationId, ConversationUnread, GatewayEvent, ListMlsMessagesQuery, RegisterDeviceRequest,
  UnreadResponse,
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
  async fn counts_follow_new_messages_and_unread_updates_and_resync_on_reconnect() {
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let server = FakeServer::new();
    let mut alice = enroll(&server, "Alice", &clock).await;
    let mut bob = enroll(&server, "Bob", &clock).await;
    let channel = channel_of(&mut [&mut alice, &mut bob]).await;
    let bob_id = bob.client.user_id();
    // Whatever setting up the channel left unread (the fake keeps no read positions).
    let (before, _) = counts(&bob, channel).expect("loaded by the full sync");

    alice.client.send_text(channel, None, "one").await.unwrap();
    alice.client.send_text(channel, None, "two").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    assert_eq!(
      counts(&bob, channel),
      Some((before + 2, 0)),
      "counted from message_create"
    );
    assert_eq!(
      counts(&alice, channel),
      Some((0, 0)),
      "own messages are not unread"
    );

    server.publish_event(
      &[bob_id],
      &GatewayEvent::UnreadUpdate {
        conversation_id: channel,
        unread_count: 5,
        mention_count: 1,
      },
    );
    settle(&mut [&mut bob]).await;
    assert_eq!(counts(&bob, channel), Some((5, 1)), "the server's word wins");

    // A dropped connection resumes; the counts are read once anyway (the fake reports nothing
    // unread), and the mark tells the UI to read its part again.
    let connections = read_state::marks(bob.store.as_ref()).unwrap().connections;
    server.drop_gateways(bob_id);
    let (gateway, outcome) = bob.client.connect_gateway().await.unwrap();
    assert_eq!(outcome, teamcraft_core::client::GatewayOutcome::Resumed);
    bob.gateway = gateway;
    assert_eq!(counts(&bob, channel), Some((0, 0)));
    assert_eq!(
      read_state::marks(bob.store.as_ref()).unwrap().connections,
      connections + 1
    );
  }

  #[tokio::test]
  async fn a_resume_counts_the_replayed_messages_once() {
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let server = FakeServer::new();
    let mut alice = enroll(&server, "Alice", &clock).await;
    let mut bob = enroll(&server, "Bob", &clock).await;
    let channel = channel_of(&mut [&mut alice, &mut bob]).await;
    let bob_id = bob.client.user_id();

    // Bob's connection drops; two messages arrive meanwhile and wait in his session's replay.
    server.drop_gateways(bob_id);
    alice.client.send_text(channel, None, "one").await.unwrap();
    alice.client.send_text(channel, None, "two").await.unwrap();
    settle(&mut [&mut alice]).await;
    // The server's count already includes both missed messages.
    server.set_unread(
      bob_id,
      UnreadResponse {
        conversations: vec![ConversationUnread {
          conversation_id: channel,
          unread_count: 7,
          mention_count: 1,
        }],
        unread_thread_count: 0,
        thread_mention_count: 0,
        maybe_max_counted: Some(1000),
        maybe_counted_through: None,
      },
    );

    let connections = read_state::marks(bob.store.as_ref()).unwrap().connections;
    let (gateway, outcome) = bob.client.connect_gateway().await.unwrap();
    assert_eq!(outcome, teamcraft_core::client::GatewayOutcome::Resumed);
    bob.gateway = gateway;
    settle(&mut [&mut bob]).await;
    assert_eq!(
      counts(&bob, channel),
      Some((7, 1)),
      "the replayed message_create events are not counted on top of the server's count"
    );
    assert_eq!(
      read_state::marks(bob.store.as_ref()).unwrap().connections,
      connections + 1,
      "one resync, after the replay"
    );
    let messages = bob
      .store
      .messages(channel, teamcraft_core::store::MessageFilter::All)
      .unwrap();
    assert!(messages.len() >= 2, "the replay itself was applied");
  }

  #[tokio::test]
  async fn announcement_updates_move_the_mark() {
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let server = FakeServer::new();
    let mut alice = enroll(&server, "Alice", &clock).await;
    server.publish_event(&[alice.client.user_id()], &GatewayEvent::AnnouncementUpdate);
    settle(&mut [&mut alice]).await;
    assert_eq!(read_state::marks(alice.store.as_ref()).unwrap().announcements, 1);
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

/// The user's own devices in the settings (issue #409) and the key backup state (issue #410).
#[cfg(feature = "ui")]
mod own_devices {
  use teamcraft_client_api::security::RecoveryMethod;
  use teamcraft_core::client::OwnDeviceCheck;
  use teamcraft_core::client::devices::{OWN_DEVICES_KEY, OwnDevicesRecord};
  use teamcraft_core::records;
  use teamcraft_core::ui_snapshot::key_backup_state;
  use teamcraft_proto::{PushProvider, RegisterPushRequest, TestPushOutcome};

  use super::*;

  const START_MS: i64 = 1_800_000_000_000;

  #[tokio::test]
  async fn the_users_devices_are_verified_and_one_the_server_made_up_is_not() {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(START_MS));
    let mut alice = enroll(&server, "Alice", &clock).await;
    let laptop = link_new_device(&mut alice, &clock).await;
    // Only the server's word: a device with a certificate the identity key never signed.
    let made_up = alice
      .api
      .register_device(&RegisterDeviceRequest {
        name: "Made up".into(),
        public_key: Base64Bytes::new(vec![1; 32]),
        cross_signature: Base64Bytes::new(vec![2; 64]),
      })
      .await
      .unwrap();
    // A real certificate of the user, copied onto another device the server lists.
    let listed = alice.api.list_user_devices(alice.client.user_id()).await.unwrap();
    let laptop_entry = listed
      .devices
      .iter()
      .find(|device| device.id == laptop.device_id())
      .unwrap();
    let copied = alice
      .api
      .register_device(&RegisterDeviceRequest {
        name: "Copied".into(),
        public_key: laptop_entry.public_key.clone(),
        cross_signature: laptop_entry.cross_signature.clone(),
      })
      .await
      .unwrap();

    alice.client.refresh_own_devices().await.unwrap();
    let record = own_devices(&alice);
    assert_eq!(record.devices.len(), 4);
    assert_eq!(state_of(&record, copied.id), OwnDeviceCheck::Unverified);
    assert_eq!(
      state_of(&record, alice.client.device_id()),
      OwnDeviceCheck::Verified
    );
    assert_eq!(state_of(&record, laptop.device_id()), OwnDeviceCheck::Verified);
    assert_eq!(state_of(&record, made_up.id), OwnDeviceCheck::Unverified);
  }

  #[tokio::test]
  async fn a_renamed_device_and_a_test_push_show_in_the_list() {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(START_MS));
    let mut alice = enroll(&server, "Alice", &clock).await;
    let me = alice.client.device_id();

    alice
      .client
      .rename_own_device(me, "  Work laptop ")
      .await
      .unwrap();
    let record = own_devices(&alice);
    assert_eq!(record.devices[0].name, "Work laptop");
    let push = record.maybe_push.as_ref().unwrap();
    assert_eq!(push[0].maybe_provider, None);

    alice
      .api
      .register_push(
        me,
        &RegisterPushRequest {
          provider: PushProvider::Fcm,
          token: "device-token".into(),
          push_key: Base64Bytes::new(vec![7; 32]),
        },
      )
      .await
      .unwrap();
    alice.client.test_push_own_device(me).await.unwrap();
    let record = own_devices(&alice);
    assert_eq!(record.tests.len(), 1);
    assert_eq!(
      (record.tests[0].device, &record.tests[0].outcome),
      (me, &TestPushOutcome::Sent)
    );
    let push = record.maybe_push.as_ref().unwrap();
    assert_eq!(push[0].maybe_provider, Some(PushProvider::Fcm));
    // The outcome stays when the list is read again.
    alice.client.refresh_own_devices().await.unwrap();
    assert_eq!(own_devices(&alice).tests, record.tests);
  }

  #[tokio::test]
  async fn the_backup_state_names_the_passphrase_and_when_it_was_last_set() {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(START_MS));
    let mut alice = enroll(&server, "Alice", &clock).await;
    let state = key_backup_state(alice.client.store()).unwrap().unwrap();
    assert!(state.is_stored);
    assert_eq!(state.recovery, [RecoveryMethod::Passphrase]);
    assert_eq!(state.maybe_passphrase_set_at, Some(START_MS));

    clock.advance(60_000);
    alice
      .client
      .upload_key_bag_with(
        &SecretBytes::new(b"a newer passphrase".to_vec()),
        options(PASSPHRASE).argon2,
      )
      .await
      .unwrap();
    let state = key_backup_state(alice.client.store()).unwrap().unwrap();
    assert_eq!(state.maybe_passphrase_set_at, Some(START_MS + 60_000));
    assert!(!state.has_pending_upload, "uploaded with the new wrap");

    // The user's other device learns it with the key bag.
    let mut laptop = link_new_device(&mut alice, &clock).await;
    laptop.sync_account_data().await.unwrap();
    let on_laptop = key_backup_state(laptop.store()).unwrap().unwrap();
    assert_eq!(on_laptop.maybe_passphrase_set_at, Some(START_MS + 60_000));
  }

  #[test]
  fn without_known_wraps_the_state_is_unknown() {
    assert_eq!(key_backup_state(&MemoryStore::new()).unwrap(), None);
  }

  fn own_devices(peer: &Peer) -> OwnDevicesRecord {
    records::get_json(peer.store.as_ref(), Table::SyncState, OWN_DEVICES_KEY)
      .unwrap()
      .unwrap()
  }

  fn state_of(record: &OwnDevicesRecord, device: teamcraft_proto::DeviceId) -> OwnDeviceCheck {
    record
      .verification
      .iter()
      .find(|(id, _)| *id == device)
      .map(|(_, state)| *state)
      .unwrap()
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
  let new_store = Arc::new(MemoryStore::new());
  let (approved, received) = tokio::join!(
    owner.client.complete_link_approval(owner_side.unwrap()),
    new_side.unwrap().receive_secrets(&new_store, "Test device")
  );
  approved.unwrap();
  let link_options = linked_options(received.unwrap());
  PendingEnrollment::new()
    .complete(
      api,
      new_store,
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

/// Unread and mention counts of the conversation as the core stores them; `None` before any sync.
fn counts(peer: &Peer, conversation: ConversationId) -> Option<(u32, u32)> {
  read_state::unread(peer.store.as_ref()).unwrap().map(|unread| {
    unread
      .conversations
      .iter()
      .find(|entry| entry.conversation_id == conversation)
      .map_or((0, 0), |entry| (entry.unread_count, entry.mention_count))
  })
}
