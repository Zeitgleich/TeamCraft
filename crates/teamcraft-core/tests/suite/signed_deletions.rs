//! Signed deletions of channels and spaces (issue #1135, e2ee.md §4.6) between clients and the fake
//! server: a real deletion purges at once, online and for a device that was away, once; and every
//! false, stale or misdirected claim purges nothing. The verifying rules case by case are in
//! `teamcraft_core::signed_deletion`.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settle};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::{LocalStore, MessageFilter, Table};
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::{
  ChannelDeletionDocument, ChannelRole, ConversationId, DeleteConversationRequest, GatewayEvent, SpaceRole,
};

mod real_deletions {
  use super::*;

  #[tokio::test]
  async fn a_channel_admin_deletes_and_a_member_online_purges_at_once() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut alice, &mut bob).await;

    alice.client.delete_channel(channel).await.unwrap();
    assert!(server.deletion_signature(channel).is_some());
    settle(&mut [&mut bob]).await;

    assert_purged(&bob, channel);
    assert!(!bob.client.has_group(channel).unwrap());
  }

  /// Bob doesn't read his gateway: the sync's list brings the signature. Again and again it finds
  /// nothing left.
  #[tokio::test]
  async fn a_device_that_was_away_catches_up_and_a_replay_finds_nothing_left() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut alice, &mut bob).await;

    alice.client.delete_channel(channel).await.unwrap();
    bob.client.full_sync().await.unwrap();
    assert_purged(&bob, channel);

    bob.client.full_sync().await.unwrap();
    assert_purged(&bob, channel);
    settle(&mut [&mut bob]).await;
    assert_purged(&bob, channel);
    assert!(!suspect_notice(&bob), "a real deletion is no suspect claim");
  }

  /// The signer's role came after Bob last read the log: it stays readable for him after the
  /// deletion, so he verifies it.
  #[tokio::test]
  async fn a_device_missing_the_signers_promotion_reads_the_log_of_the_deleted_channel() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    add(&mut alice, &mut bob, &mut carol, channel).await;
    alice
      .client
      .set_channel_role(channel, carol.client.user_id(), ChannelRole::Admin)
      .await
      .unwrap();
    assert_ne!(
      bob
        .client
        .verified_channel_role(channel, carol.client.user_id())
        .unwrap(),
      ChannelRole::Admin,
      "Bob hasn't read it"
    );
    carol.client.sync_channel_roles(channel).await.unwrap();

    carol.client.delete_channel(channel).await.unwrap();
    bob.client.full_sync().await.unwrap();

    assert_purged(&bob, channel);
  }

  /// Multi-device: the deleter's other device holds nothing but the signature, like anybody's.
  #[tokio::test]
  async fn the_deleters_other_device_verifies_it_too() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    let mut phone = common::link_device(&mut alice).await;
    phone.full_sync().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    phone.full_sync().await.unwrap();

    assert!(phone.has_group(channel).unwrap());
    alice.client.delete_channel(channel).await.unwrap();
    phone.full_sync().await.unwrap();
    assert!(!phone.has_group(channel).unwrap());
    assert!(
      phone
        .store()
        .messages(channel, MessageFilter::TopLevel)
        .unwrap()
        .is_empty()
    );
  }

  /// A workspace admin outside the channel signs; the first answer tells them the group's epoch.
  #[tokio::test]
  async fn a_workspace_admin_outside_the_channel_learns_the_epoch_and_signs() {
    let server = FakeServer::new();
    let (mut owner, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut owner, &mut bob).await;
    let mut admin = Peer::enroll(&server, "Admin").await;
    make_workspace_admin(&server, &mut owner, &mut admin).await;
    // Past the group's first epoch, which is what the admin's device would sign by itself.
    server.bump_epoch(channel, 1);
    bob.client.full_sync().await.unwrap();

    // Admin's device knows the channel only from the server's answer here.
    admin.client.delete_channel(channel).await.unwrap();

    let signed = server.deletion_signature(channel).unwrap();
    let document: ChannelDeletionDocument = serde_json::from_slice(signed.payload.as_slice()).unwrap();
    assert!(document.epoch > 0, "not the 0 of a device outside the group");
    assert!(!admin.client.has_group(channel).unwrap());
    bob.client.full_sync().await.unwrap();
    assert_purged(&bob, channel);
  }

  /// A commit landed between what the admin's device knew and the request.
  #[tokio::test]
  async fn an_epoch_conflict_is_signed_again() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    let before = epoch_of(&mut bob, channel);
    server.bump_epoch(channel, 2);

    alice.client.delete_channel(channel).await.unwrap();

    let signed = server.deletion_signature(channel).unwrap();
    let document: ChannelDeletionDocument = serde_json::from_slice(signed.payload.as_slice()).unwrap();
    assert_eq!(document.epoch, before + 2);
    bob.client.full_sync().await.unwrap();
    assert_purged(&bob, channel);
  }
}

mod false_claims {
  use super::*;

  /// On the gateway too: a forged signature on the event hides, and nothing more.
  #[tokio::test]
  async fn a_forged_signature_on_the_gateway_purges_nothing() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let carol = Peer::enroll(&server, "Carol").await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    let epoch = epoch_of(&mut bob, channel);
    let forged = carol.client.sign_channel_deletion(channel, epoch).unwrap();
    let bob_id = bob.client.user_id();
    server.hide_from(bob_id, channel);

    server.publish_event(
      &[bob_id],
      &GatewayEvent::ConversationDelete {
        conversation_id: channel,
        maybe_signed_deletion: Some(forged),
      },
    );
    settle(&mut [&mut bob]).await;

    assert_hidden_with_history(&bob, channel);
  }

  #[tokio::test]
  async fn a_member_signing_it_purges_nothing() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    add(&mut alice, &mut bob, &mut carol, channel).await;
    let epoch = epoch_of(&mut bob, channel);

    let forged = carol.client.sign_channel_deletion(channel, epoch).unwrap();
    server.claim_signed_deletion(channel, forged);
    server.hide_from(bob.client.user_id(), channel);
    bob.client.full_sync().await.unwrap();

    assert_hidden_with_history(&bob, channel);
  }

  /// Bob's device accepted the demotion: the old admin's signature counts for nothing.
  #[tokio::test]
  async fn a_demoted_admin_purges_nothing() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    add(&mut alice, &mut bob, &mut carol, channel).await;
    let carol_id = carol.client.user_id();
    alice
      .client
      .set_channel_role(channel, carol_id, ChannelRole::Admin)
      .await
      .unwrap();
    carol.client.sync_channel_roles(channel).await.unwrap();
    alice
      .client
      .set_channel_role(channel, carol_id, ChannelRole::Member)
      .await
      .unwrap();
    bob.client.sync_channel_roles(channel).await.unwrap();
    let epoch = epoch_of(&mut bob, channel);

    let demoted = carol.client.sign_channel_deletion(channel, epoch).unwrap();
    server.claim_signed_deletion(channel, demoted);
    server.hide_from(bob.client.user_id(), channel);
    bob.client.full_sync().await.unwrap();

    assert_hidden_with_history(&bob, channel);
  }

  /// A real admin's signature, held back; the group moved on since: replayed, it's refused.
  #[tokio::test]
  async fn a_replayed_action_from_an_older_epoch_purges_nothing() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    let epoch = epoch_of(&mut bob, channel);
    let held_back = alice.client.sign_channel_deletion(channel, epoch).unwrap();
    add(&mut alice, &mut bob, &mut carol, channel).await;
    assert!(epoch_of(&mut bob, channel) > epoch);

    server.claim_signed_deletion(channel, held_back);
    server.hide_from(bob.client.user_id(), channel);
    bob.client.full_sync().await.unwrap();

    assert_hidden_with_history(&bob, channel);
  }

  #[tokio::test]
  async fn a_real_signature_for_another_conversation_purges_nothing() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    let space = alice.space().await;
    let other = alice.client.create_channel(space, "other").await.unwrap();
    settle(&mut [&mut alice]).await;
    let epoch = epoch_of(&mut bob, channel);

    let elsewhere = alice.client.sign_channel_deletion(other, epoch).unwrap();
    server.claim_signed_deletion(channel, elsewhere);
    server.hide_from(bob.client.user_id(), channel);
    bob.client.full_sync().await.unwrap();

    assert_hidden_with_history(&bob, channel);
  }

  /// A real signature about a channel the server goes on listing: forged or replayed.
  #[tokio::test]
  async fn a_signed_claim_about_what_it_still_lists_changes_nothing() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    let epoch = epoch_of(&mut bob, channel);

    let signed = alice.client.sign_channel_deletion(channel, epoch).unwrap();
    server.claim_signed_deletion(channel, signed);
    server.claim_deleted(bob.client.user_id(), channel);
    bob.client.full_sync().await.unwrap();

    assert!(bob.client.has_group(channel).unwrap());
    assert!(has_history(&bob, channel));
  }

  /// No signature: what an older client or a server before signed deletions gives. The retention.
  #[tokio::test]
  async fn an_unsigned_claim_keeps_the_history() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    alice
      .api
      .delete_conversation(channel, &DeleteConversationRequest::default())
      .await
      .unwrap();
    assert!(server.deletion_signature(channel).is_none());

    bob.client.full_sync().await.unwrap();

    assert_hidden_with_history(&bob, channel);
  }

  /// The group went with a first, unsigned claim: the device can't tell how old a later signature
  /// is, so the retention stands, and the signature is judged without asking the server.
  #[tokio::test]
  async fn a_signature_after_a_claim_took_the_group_keeps_the_history() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    let epoch = epoch_of(&mut bob, channel);
    server.hide_from(bob.client.user_id(), channel);
    bob.client.full_sync().await.unwrap();
    assert_hidden_with_history(&bob, channel);

    let signed = alice.client.sign_channel_deletion(channel, epoch).unwrap();
    server.claim_signed_deletion(channel, signed);
    bob.client.full_sync().await.unwrap();

    assert_hidden_with_history(&bob, channel);
  }

  /// Notes to self have no signed roles: a workspace admin's signature doesn't reach them.
  #[tokio::test]
  async fn notes_to_self_are_never_purged_by_a_signature() {
    let server = FakeServer::new();
    let (mut owner, mut bob) = two(&server).await;
    let mut admin = Peer::enroll(&server, "Admin").await;
    make_workspace_admin(&server, &mut owner, &mut admin).await;
    bob.client.full_sync().await.unwrap();
    let bob_id = bob.client.user_id();
    let notes = bob.client.open_dm(bob_id).await.unwrap();
    bob.client.send_text(notes, None, "note").await.unwrap();
    settle(&mut [&mut bob]).await;
    let epoch = epoch_of(&mut bob, notes);

    let signed = admin.client.sign_channel_deletion(notes, epoch).unwrap();
    server.claim_signed_deletion(notes, signed);
    server.hide_from(bob.client.user_id(), notes);
    bob.client.full_sync().await.unwrap();

    assert!(has_history(&bob, notes));
  }
}

/// A verified purge cut off leaves its mark, without the group that made it verifiable: the next
/// claim finishes it.
#[tokio::test]
async fn a_verified_purge_that_was_cut_off_is_finished() {
  let server = FakeServer::new();
  let (mut alice, mut bob) = two(&server).await;
  let channel = channel_with_history(&mut alice, &mut bob).await;
  bob
    .store
    .put(Table::AppliedChanges, &format!("own_deletion/{channel}"), b"1")
    .unwrap();

  server.hide_from(bob.client.user_id(), channel);
  bob.client.full_sync().await.unwrap();

  assert_purged(&bob, channel);
}

mod deleting_a_space {
  use super::*;

  /// A workspace admin signs each channel; the server's first answer names the epochs of those their
  /// device isn't in.
  #[tokio::test]
  async fn a_workspace_admin_signs_every_channel_their_device_knows() {
    let server = FakeServer::new();
    let (mut owner, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut owner, &mut bob).await;
    let space = owner.space().await;
    server.set_admin(owner.client.user_id());
    owner.client.delete_space(space).await.unwrap();

    assert!(server.deletion_signature(channel).is_some());
    bob.client.full_sync().await.unwrap();
    assert_purged(&bob, channel);
  }

  /// A space admin whose role isn't a signed fact: the deletion goes, unsigned, so the history stays.
  #[tokio::test]
  async fn a_space_admin_without_a_signed_authority_leaves_a_claim() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    let space = alice.space().await;
    alice
      .client
      .set_space_role(space, bob.client.user_id(), SpaceRole::SpaceAdmin)
      .await
      .unwrap();

    bob.client.delete_space(space).await.unwrap();
    assert!(server.deletion_signature(channel).is_none());
    alice.client.full_sync().await.unwrap();

    assert_hidden_with_history(&alice, channel);
  }
}

async fn two(server: &FakeServer) -> (Peer, Peer) {
  (
    Peer::enroll(server, "Alice").await,
    Peer::enroll(server, "Bob").await,
  )
}

/// Alice's channel with Bob in it and a message both hold; her founding entry signs her admin, and
/// Bob verified it.
async fn channel_with_history(alice: &mut Peer, bob: &mut Peer) -> ConversationId {
  let space = alice.space().await;
  let channel = alice.client.create_channel(space, "general").await.unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [alice, bob]).await;
  bob.client.sync_channel_roles(channel).await.unwrap();
  alice.client.send_text(channel, None, "hello").await.unwrap();
  settle(&mut [alice, bob]).await;
  channel
}

/// Carol joins Alice's channel, which moves its group to the next epoch for all of them.
async fn add(alice: &mut Peer, bob: &mut Peer, carol: &mut Peer, channel: ConversationId) {
  alice
    .client
    .add_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [alice, bob, carol]).await;
}

async fn make_workspace_admin(server: &FakeServer, owner: &mut Peer, admin: &mut Peer) {
  let (owner_id, admin_id) = (owner.client.user_id(), admin.client.user_id());
  owner
    .client
    .publish_roster(&[(owner_id, RosterRole::Owner), (admin_id, RosterRole::Admin)])
    .await
    .unwrap();
  server.set_admin(admin_id);
  admin.client.full_sync().await.unwrap();
}

fn epoch_of(peer: &mut Peer, channel: ConversationId) -> u64 {
  peer.client.group_epoch(channel).unwrap().unwrap()
}

fn has_history(peer: &Peer, channel: ConversationId) -> bool {
  !peer
    .store
    .messages(channel, MessageFilter::TopLevel)
    .unwrap()
    .is_empty()
}

fn is_cached(peer: &Peer, channel: ConversationId) -> bool {
  peer
    .store
    .get(Table::Conversations, &channel.to_string())
    .unwrap()
    .is_some()
}

fn suspect_notice(peer: &Peer) -> bool {
  peer
    .store
    .get(Table::AppliedChanges, "suspect_deletions")
    .unwrap()
    .is_some()
}

fn assert_purged(peer: &Peer, channel: ConversationId) {
  assert!(!is_cached(peer, channel));
  assert!(!has_history(peer, channel), "the history goes at once");
  assert!(
    peer
      .store
      .list(Table::AppliedChanges)
      .unwrap()
      .iter()
      .all(|(key, _)| !key.ends_with(&channel.to_string())),
    "no mark is left"
  );
}

fn assert_hidden_with_history(peer: &Peer, channel: ConversationId) {
  assert!(!is_cached(peer, channel));
  assert!(has_history(peer, channel), "the history stays for its retention");
}
