//! The channel anchor between clients and the fake server (e2ee.md §4.6.5, issue #1740): founding
//! anchored once the server's minimum client version reaches the anchoring release, the pin and
//! floor in the account data on every member's device, renames raising it, the name rules at the
//! field, and the name a deletion names (D3). The verdicts case by case are in
//! `teamcraft_core::anchoring` and `teamcraft_crypto::channel_anchor`.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{Peer, settle};
use teamcraft_core::anchoring::ANCHORING_RELEASE;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::RoleSigning;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::error::CoreError;
use teamcraft_core::records;
use teamcraft_core::store::{LocalStore, MessageFilter, Table};
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::channel_name::ChannelNameError;
use teamcraft_proto::{
  ChannelRole, ConversationId, ConversationKind, ConversationResponse, UpdateConversationRequest,
};

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

/// What the apps show of the anchor (issue #1749): who last named the channel, a name the server
/// lists but the anchor doesn't hold, and a channel the pin keeps this device out of.
mod marks {
  use super::*;
  use teamcraft_core::anchoring::NameRole;
  use teamcraft_core::client::{channel_anchor_marks, is_anchor_refused};
  use teamcraft_crypto::channel_anchor::{AnchorFloor, AnchorHash, FloorState};

  /// Alice is no workspace admin: her rename is signed as a member's, and Bob's device names her.
  /// A name the server then lists on its own is stale there.
  #[tokio::test]
  async fn a_members_rename_is_named_and_a_server_only_name_is_stale() {
    let server = anchoring_server();
    let (mut alice, mut bob) = two(&server).await;
    let alice_id = alice.client.user_id();
    let channel = shared_channel(&mut alice, &mut bob, "general").await;
    let marks = channel_anchor_marks(&*bob.store, channel).unwrap().unwrap();
    assert_eq!(marks.maybe_name_set_by, Some((alice_id, NameRole::Founder)));
    assert!(!marks.is_name_stale);

    rename(&mut alice, channel, "announcements").await;
    settle(&mut [&mut alice, &mut bob]).await;
    let marks = channel_anchor_marks(&*bob.store, channel).unwrap().unwrap();
    assert_eq!(marks.maybe_name_set_by, Some((alice_id, NameRole::Member)));
    assert!(!marks.is_name_stale);
    assert_eq!(marks.maybe_fork, None);

    let mut listed: ConversationResponse =
      records::get_json(&*bob.store, Table::Conversations, &channel.to_string())
        .unwrap()
        .unwrap();
    listed.maybe_name = Some("old-test".to_owned());
    records::put_json(&*bob.store, Table::Conversations, &channel.to_string(), &listed).unwrap();
    assert!(
      channel_anchor_marks(&*bob.store, channel)
        .unwrap()
        .unwrap()
        .is_name_stale
    );
  }

  /// Bob's devices pinned another founding for this channel: the welcome into Alice's group is
  /// refused, and the channel shows as unreachable instead of waiting silently.
  #[tokio::test]
  async fn a_welcome_the_pin_refuses_leaves_the_channel_unreachable() {
    let server = anchoring_server();
    let (mut alice, mut bob) = two(&server).await;
    let space = alice.space().await;
    let channel = alice.client.create_channel(space, "general").await.unwrap();
    let other = AnchorFloor {
      founding_hash: AnchorHash::from_bytes([9; 32]),
      properties_version: 1,
      properties_hash: AnchorHash::from_bytes([9; 32]),
      epoch: 0,
      state: FloorState::Agreed,
    };
    bob
      .client
      .update_account_data(|data, _| data.note_anchor_floor(channel, &other))
      .unwrap();
    assert!(!is_anchor_refused(&*bob.store, channel).unwrap());
    alice
      .client
      .add_member(channel, bob.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    assert!(!bob.client.has_group(channel).unwrap(), "stays out");
    assert!(is_anchor_refused(&*bob.store, channel).unwrap());
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

/// e2ee.md §4.6.5, "What the founding decides": the role log of an anchored channel takes only the
/// anchor's founding, and a snapshot going back to it is anchored (#1748).
mod roles {
  use super::*;

  /// Alice is no workspace admin: her founding anchors all the same, on Bob's device and her own.
  #[tokio::test]
  async fn a_members_anchored_founding_anchors_the_roles() {
    let server = anchoring_server();
    let (mut alice, mut bob) = two(&server).await;
    let channel = shared_channel(&mut alice, &mut bob, "general").await;
    let alice_id = alice.client.user_id();
    bob.client.sync_channel_roles(channel).await.unwrap();
    for peer in [&alice, &bob] {
      let state = peer.client.channel_role_state(channel).unwrap();
      assert_eq!(state.role_of(alice_id), ChannelRole::Admin);
      assert!(state.is_workspace_anchored, "{}", peer.name);
    }
  }

  /// Review of #1748: the server withholds Alice's device list before Bob's device ever pinned the
  /// anchor. Bob's device can't verify it, but the group carries it: Alice's founding is no claim
  /// to take on first use. It waits unread, the log isn't fetched again at once, and once the list
  /// is back and the wait is over, the founding is read and anchors.
  #[tokio::test]
  async fn an_unverifiable_anchor_without_a_pin_founds_nothing_until_it_verifies() {
    let server = anchoring_server();
    let clock = Arc::new(ManualClock::new(server.server_now_ms()));
    let enroll = async |name: &str| {
      Peer::enroll_on_clock(
        server.add_user(name),
        name,
        common::settings(),
        Box::new(clock.clone()),
      )
      .await
    };
    let (mut alice, mut bob) = (enroll("Alice").await, enroll("Bob").await);
    let alice_id = alice.client.user_id();
    let space = alice.space().await;
    let channel = alice.client.create_channel(space, "general").await.unwrap();
    let list = server.device_list_of(alice_id).unwrap();
    server.forget_device_list(alice_id);
    alice
      .client
      .add_member(channel, bob.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    assert!(bob.client.has_group(channel).unwrap());
    assert_eq!(
      bob.client.account_data().anchor_floor(channel),
      None,
      "never pinned"
    );

    bob.client.sync_channel_roles(channel).await.unwrap();
    let state = bob.client.channel_role_state(channel).unwrap();
    assert!(!state.is_signed(), "Alice's founding is no claim here");
    assert_eq!(state.role_of(alice_id), ChannelRole::Member);
    let reads = server.role_log_reads(channel);
    bob.client.sync_channel_roles(channel).await.unwrap();
    assert_eq!(server.role_log_reads(channel), reads, "asked again only later");

    server.serve_device_list(alice_id, list);
    clock.advance(6 * 60 * 1000);
    bob.client.sync_channel_roles(channel).await.unwrap();
    let state = bob.client.channel_role_state(channel).unwrap();
    assert_eq!(state.role_of(alice_id), ChannelRole::Admin);
    assert!(state.is_workspace_anchored);
  }

  /// Review of #1748: Alice's founding entry never reached the server at creation. Her own upkeep
  /// signs it later, as the anchor's founder; it is no founding at first sight. Also where she is
  /// the workspace's owner: while the founding is open only the founder's entry counts, never a
  /// workspace admin's, and that is what she signs.
  #[tokio::test]
  async fn a_founding_lost_at_creation_is_signed_by_the_founders_upkeep() {
    for is_workspace_owner in [false, true] {
      let server = anchoring_server();
      let (mut alice, mut bob) = two(&server).await;
      let alice_id = alice.client.user_id();
      if is_workspace_owner {
        alice
          .client
          .publish_roster(&[(alice_id, RosterRole::Owner)])
          .await
          .unwrap();
        bob.client.full_sync().await.unwrap();
      }
      server.set_role_log_lacking(true);
      let channel = shared_channel(&mut alice, &mut bob, "general").await;
      server.set_role_log_lacking(false);
      assert_eq!(server.role_log(channel).len(), 0);

      bob.client.found_listed_channels().await.unwrap();
      assert_eq!(server.role_log(channel).len(), 0, "Bob isn't the founder");
      let bob_id = bob.client.user_id();
      if is_workspace_owner {
        // Her next role change founds on the way: as founder, never as the workspace's owner.
        let signing = alice
          .client
          .set_channel_role(channel, bob_id, ChannelRole::Admin)
          .await
          .unwrap();
        assert_eq!(signing, RoleSigning::Signed);
      } else {
        alice.client.found_listed_channels().await.unwrap();
      }
      assert_eq!(server.role_log(channel).len(), 1, "owner: {is_workspace_owner}");
      bob.client.sync_channel_roles(channel).await.unwrap();
      let state = bob.client.channel_role_state(channel).unwrap();
      assert_eq!(
        state.role_of(alice_id),
        ChannelRole::Admin,
        "owner: {is_workspace_owner}"
      );
      if is_workspace_owner {
        assert_eq!(state.role_of(bob_id), ChannelRole::Admin);
      }
      assert!(state.is_workspace_anchored);
      assert!(!state.has_server_listed_roles);
    }
  }

  /// The server lists Bob as the channel's admin and shows his device an empty log: neither his
  /// upkeep nor his next role change founds a second one (no first sight in an anchored channel).
  #[tokio::test]
  async fn a_servers_empty_log_at_cursor_zero_founds_nothing() {
    let server = anchoring_server();
    let (mut alice, mut bob) = two(&server).await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let bob_id = bob.client.user_id();
    let space = alice.space().await;
    let channel = alice.client.create_channel(space, "general").await.unwrap();
    server.withhold_role_log(bob_id, channel, 0);
    for peer in [&bob, &carol] {
      alice
        .client
        .add_member(channel, peer.client.user_id())
        .await
        .unwrap();
    }
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert_eq!(server.role_log(channel).len(), 1);
    server.set_channel_role_unsigned(channel, bob_id, ChannelRole::Admin);
    bob.client.full_sync().await.unwrap();
    bob.client.sync_channel_roles(channel).await.unwrap();
    assert!(bob.client.channel_role_state(channel).unwrap().is_unfounded());

    bob.client.found_listed_channels().await.unwrap();
    let signing = bob
      .client
      .set_channel_role(channel, carol.client.user_id(), ChannelRole::Moderator)
      .await
      .unwrap();
    assert_eq!(signing, RoleSigning::Unsigned);
    assert_eq!(server.role_log(channel).len(), 1, "nothing founded");
  }

  /// D8: Alice, a member, founded the channel and made Bob an admin. Bob's deletion purges at once
  /// on Carol's device while Alice's entry is younger than §4.6.3's time limit, and takes the
  /// retention after it.
  #[tokio::test]
  async fn a_channel_admin_of_a_members_channel_purges_within_the_time_limit() {
    for (waited, is_purged) in [(0, true), (31 * 24 * 60 * 60, false)] {
      let server = anchoring_server();
      let clock = Arc::new(ManualClock::new(server.server_now_ms()));
      let enroll = async |name: &str| {
        Peer::enroll_on_clock(
          server.add_user(name),
          name,
          common::settings(),
          Box::new(clock.clone()),
        )
        .await
      };
      let (mut alice, mut bob, mut carol) =
        (enroll("Alice").await, enroll("Bob").await, enroll("Carol").await);
      let channel = shared_channel(&mut alice, &mut bob, "general").await;
      alice
        .client
        .add_member(channel, carol.client.user_id())
        .await
        .unwrap();
      settle(&mut [&mut alice, &mut bob, &mut carol]).await;
      let bob_id = bob.client.user_id();
      let signing = alice
        .client
        .set_channel_role(channel, bob_id, ChannelRole::Admin)
        .await
        .unwrap();
      assert_eq!(signing, RoleSigning::Signed);
      alice.client.send_text(channel, None, "hello").await.unwrap();
      settle(&mut [&mut alice, &mut bob, &mut carol]).await;
      for peer in [&mut bob, &mut carol] {
        peer.client.sync_channel_roles(channel).await.unwrap();
      }
      let roles = carol.client.channel_role_state(channel).unwrap();
      assert_eq!(roles.role_of(bob_id), ChannelRole::Admin);
      assert!(roles.is_workspace_anchored);

      let wait = std::time::Duration::from_secs(waited);
      clock.advance(i64::try_from(wait.as_millis()).unwrap());
      server.advance_clock(wait);
      bob.client.delete_channel(channel).await.unwrap();
      assert!(server.deletion_signature(channel).is_some());
      carol.client.full_sync().await.unwrap();
      let has_history = !carol
        .store
        .messages(channel, MessageFilter::TopLevel)
        .unwrap()
        .is_empty();
      assert_eq!(!has_history, is_purged, "after {waited} s");
    }
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
