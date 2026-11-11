//! The channel anchor between clients and the fake server (e2ee.md §4.6.5, issue #1740): founding
//! anchored once the server's minimum client version reaches the anchoring release, the pin and
//! floor in the account data on every member's device, renames raising it, the name rules at the
//! field, and the name a deletion names (D3). The verdicts case by case are in
//! `teamcraft_core::anchoring` and `teamcraft_crypto::channel_anchor`.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settle};
use teamcraft_core::anchoring::ANCHORING_RELEASE;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::error::CoreError;
use teamcraft_core::records;
use teamcraft_core::store::{LocalStore, Table};
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::channel_name::ChannelNameError;
use teamcraft_proto::{ConversationId, ConversationKind, ConversationResponse, UpdateConversationRequest};

mod founding {
  use super::*;

  /// D2: before the server names the release as the minimum, channels are founded as before.
  #[tokio::test]
  async fn a_channel_is_founded_unanchored_below_the_release() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let space = alice.space().await;
    let channel = alice.client.create_channel(space, "general").await.unwrap();
    assert_eq!(alice.client.account_data().anchor_floor(channel), None);
  }

  #[tokio::test]
  async fn the_founder_and_a_member_pin_the_same_founding() {
    let server = anchoring_server();
    let (mut alice, mut bob) = two(&server).await;
    let channel = shared_channel(&mut alice, &mut bob, "general").await;
    let alice_floor = alice.client.account_data().anchor_floor(channel).unwrap();
    let bob_floor = bob.client.account_data().anchor_floor(channel).unwrap();
    assert_eq!(alice_floor.founding_hash, bob_floor.founding_hash);
    assert_eq!(bob_floor.properties_version, 1);
    assert!(bob_floor.grants());
  }

  /// Once anchoring is on, a device that can't find itself on its own device list creates no
  /// channel at all, rather than one founded unanchored without a word.
  #[tokio::test]
  async fn a_device_off_its_own_list_creates_nothing() {
    let server = anchoring_server();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let alice_id = alice.client.user_id();
    server.forget_device_list(alice_id);
    alice
      .store
      .delete(Table::DeviceLists, &alice_id.to_string())
      .unwrap();
    let space = alice.space().await;
    let before = alice.api.list_conversations().await.unwrap().conversations.len();
    let result = alice.client.create_channel(space, "general").await;
    assert!(
      matches!(result, Err(CoreError::OwnDeviceListMissing)),
      "{result:?}"
    );
    let after = alice.api.list_conversations().await.unwrap().conversations.len();
    assert_eq!(after, before, "nothing was created on the server");
  }

  /// D2: a lower minimum the server reports later doesn't turn anchoring off again.
  #[tokio::test]
  async fn the_minimum_version_only_rises_on_the_device() {
    let server = anchoring_server();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let space = alice.space().await;
    alice.client.create_channel(space, "first").await.unwrap();
    server.set_minimum_client_version("1.0.0");
    let channel = alice.client.create_channel(space, "second").await.unwrap();
    assert!(alice.client.account_data().anchor_floor(channel).is_some());
  }
}

mod joining {
  use super::*;

  /// A further device of a member joins (by external commit, or a member adds it): it checks the
  /// anchor against the pin and floor its user's account data carries, and keeps them.
  #[tokio::test]
  async fn a_further_device_joins_on_the_pinned_founding() {
    let server = anchoring_server();
    let (mut alice, mut bob) = two(&server).await;
    let channel = shared_channel(&mut alice, &mut bob, "general").await;
    let pinned = bob.client.account_data().anchor_floor(channel).unwrap();
    let mut phone = common::link_device(&mut bob).await;
    phone.full_sync().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    phone.full_sync().await.unwrap();
    assert!(phone.has_group(channel).unwrap());
    let floor = phone.account_data().anchor_floor(channel).unwrap();
    assert_eq!(floor.founding_hash, pinned.founding_hash);
    assert!(floor.epoch >= pinned.epoch);
  }
}

mod names {
  use super::*;

  #[tokio::test]
  async fn a_name_with_an_invisible_character_is_refused_before_anything_is_sent() {
    let server = anchoring_server();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let space = alice.space().await;
    let result = alice.client.create_channel(space, "gen\u{200B}eral").await;
    assert!(
      matches!(
        result,
        Err(CoreError::ChannelName(ChannelNameError::InvisibleCharacter))
      ),
      "{result:?}"
    );
  }

  /// A decomposed name is sent in NFC, so the listing and the anchor's hash agree.
  #[tokio::test]
  async fn a_decomposed_name_is_sent_composed() {
    let server = anchoring_server();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let space = alice.space().await;
    let channel = alice.client.create_channel(space, "Cafe\u{0301}").await.unwrap();
    let listed: ConversationResponse =
      records::get_json(&*alice.store, Table::Conversations, &channel.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(listed.maybe_name.as_deref(), Some("Caf\u{00E9}"));
  }

  /// A workspace admin's rename is signed into the anchor: every member's floor rises, the
  /// committer's own included.
  #[tokio::test]
  async fn a_rename_raises_every_members_floor() {
    let server = anchoring_server();
    let (mut alice, mut bob) = two(&server).await;
    let alice_id = alice.client.user_id();
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();
    let channel = shared_channel(&mut alice, &mut bob, "general").await;
    rename(&mut alice, channel, "announcements").await;
    settle(&mut [&mut alice, &mut bob]).await;
    let alice_floor = alice.client.account_data().anchor_floor(channel).unwrap();
    let bob_floor = bob.client.account_data().anchor_floor(channel).unwrap();
    assert_eq!(
      alice_floor.properties_version, 2,
      "the committer raises its own floor"
    );
    assert_eq!(bob_floor.properties_version, 2);
    assert_eq!(alice_floor.properties_hash, bob_floor.properties_hash);
  }
}

mod relisted {
  use super::*;

  /// The server relists a pinned channel as a group DM: Bob's device still checks the anchor on a
  /// commit that changes it and raises its floor, instead of skipping it on the server's word.
  #[tokio::test]
  async fn a_pinned_channel_relisted_as_a_dm_is_still_checked() {
    let server = anchoring_server();
    let (mut alice, mut bob) = two(&server).await;
    let alice_id = alice.client.user_id();
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();
    let channel = shared_channel(&mut alice, &mut bob, "general").await;
    rename(&mut alice, channel, "announcements").await;
    server.relabel_conversation(channel, ConversationKind::Group);
    bob.client.full_sync().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let floor = bob.client.account_data().anchor_floor(channel).unwrap();
    assert_eq!(
      floor.properties_version, 2,
      "checked and raised despite the relisting"
    );
  }
}

mod deletions {
  use super::*;

  /// D3: the owner's deletion names the anchored name the members hold: it purges at once.
  #[tokio::test]
  async fn a_deletion_naming_the_anchored_name_purges() {
    let server = anchoring_server();
    let (mut alice, mut bob) = two(&server).await;
    let alice_id = alice.client.user_id();
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();
    let channel = shared_channel(&mut alice, &mut bob, "general").await;
    bob.client.sync_channel_roles(channel).await.unwrap();
    alice.client.delete_channel(channel).await.unwrap();
    bob.client.full_sync().await.unwrap();
    assert!(!bob.client.has_group(channel).unwrap(), "purged at once");
  }
}

fn anchoring_server() -> FakeServer {
  let server = FakeServer::new();
  let release = ANCHORING_RELEASE;
  server.set_minimum_client_version(&format!("{}.{}.{}", release.major, release.minor, release.patch));
  server
}

async fn two(server: &FakeServer) -> (Peer, Peer) {
  (
    Peer::enroll(server, "Alice").await,
    Peer::enroll(server, "Bob").await,
  )
}

/// Alice's channel with Bob added.
async fn shared_channel(alice: &mut Peer, bob: &mut Peer, name: &str) -> ConversationId {
  let space = alice.space().await;
  let channel = alice.client.create_channel(space, name).await.unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut *alice, &mut *bob]).await;
  assert!(bob.client.has_group(channel).unwrap());
  channel
}

async fn rename(peer: &mut Peer, channel: ConversationId, name: &str) {
  peer
    .client
    .update_channel(
      channel,
      &UpdateConversationRequest {
        maybe_name: Some(name.to_owned()),
        ..UpdateConversationRequest::default()
      },
    )
    .await
    .unwrap();
}
