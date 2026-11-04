//! Signed deletions of channels and spaces (issue #1135, e2ee.md §4.6) between clients and the fake
//! server: a real deletion purges at once, online and for a device that was away, once; and every
//! false, stale or misdirected claim purges nothing. The verifying rules case by case are in
//! `teamcraft_core::signed_deletion`.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{Peer, settle};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::RoleSigning;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::store::{LocalStore, MessageFilter, Table};
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::{
  ChannelDeletionDocument, ChannelRole, ConversationId, ConversationKind, DeleteConversationRequest,
  GatewayEvent,
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

  /// Real signatures on several channels at once are worth telling, whoever signed them.
  #[tokio::test]
  async fn a_mass_purge_raises_the_notice() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let first = channel_with_history(&mut alice, &mut bob).await;
    let second = founder_rooted_channel(&mut alice, &mut bob).await;
    bob.client.sync_channel_roles(second).await.unwrap();

    alice.client.delete_channel(first).await.unwrap();
    alice.client.delete_channel(second).await.unwrap();
    bob.client.full_sync().await.unwrap();

    assert_purged(&bob, first);
    assert_purged(&bob, second);
    assert!(suspect_notice(&bob));
  }

  /// A member's device doesn't sign for an epoch far from its own group's: the signature would hold
  /// until the group got there. The deletion goes, unsigned.
  #[tokio::test]
  async fn a_member_signs_no_epoch_far_from_its_own() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    server.bump_epoch(channel, 1_000);

    alice.client.delete_channel(channel).await.unwrap();

    assert!(server.deletion_signature(channel).is_none());
    bob.client.full_sync().await.unwrap();
    assert_hidden_with_history(&bob, channel);
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

  /// The attack: a channel from before signed roles, a member, and the server. Mallory's founding
  /// entry is accepted, for roles; her signature on the deletion costs Bob nothing at once.
  #[tokio::test]
  async fn a_forged_founding_entry_purges_nothing() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let mut mallory = Peer::enroll(&server, "Mallory").await;
    server.set_role_log_lacking(true);
    let channel = founder_rooted_channel(&mut alice, &mut bob).await;
    add(&mut alice, &mut bob, &mut mallory, channel).await;
    server.set_role_log_lacking(false);
    let mallory_id = mallory.client.user_id();
    // The server plays along.
    server.set_channel_role_unsigned(channel, mallory_id, ChannelRole::Admin);

    mallory
      .client
      .sign_channel_roles(channel, &[(mallory_id, ChannelRole::Admin)], true)
      .await
      .unwrap();
    bob.client.sync_channel_roles(channel).await.unwrap();
    assert_eq!(
      bob.client.verified_channel_role(channel, mallory_id).unwrap(),
      ChannelRole::Admin
    );
    let epoch = epoch_of(&mut bob, channel);
    let forged = mallory.client.sign_channel_deletion(channel, epoch).unwrap();
    server.claim_signed_deletion(channel, forged);
    server.hide_from(bob.client.user_id(), channel);
    bob.client.full_sync().await.unwrap();

    assert_hidden_with_history(&bob, channel);
  }

  /// The price of the above: a real admin of a channel nothing but a founder vouches for.
  #[tokio::test]
  async fn a_founded_admins_real_deletion_takes_the_retention() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = founder_rooted_channel(&mut alice, &mut bob).await;

    alice.client.delete_channel(channel).await.unwrap();
    assert!(
      server.deletion_signature(channel).is_some(),
      "signed all the same"
    );
    bob.client.full_sync().await.unwrap();

    assert_hidden_with_history(&bob, channel);
  }

  /// The server calls a direct message a channel, after this device knew better.
  #[tokio::test]
  async fn a_relabelled_direct_message_purges_nothing() {
    let server = FakeServer::new();
    let (mut owner, mut bob) = two(&server).await;
    let mut admin = Peer::enroll(&server, "Admin").await;
    make_workspace_admin(&server, &mut owner, &mut admin).await;
    bob.client.full_sync().await.unwrap();
    let owner_id = owner.client.user_id();
    let direct = bob.client.open_dm(owner_id).await.unwrap();
    bob.client.send_text(direct, None, "just us").await.unwrap();
    settle(&mut [&mut bob, &mut owner]).await;
    bob.client.full_sync().await.unwrap();

    server.relabel_conversation(direct, ConversationKind::Channel);
    bob.client.full_sync().await.unwrap();
    let epoch = epoch_of(&mut bob, direct);
    let signed = admin.client.sign_channel_deletion(direct, epoch).unwrap();
    server.claim_signed_deletion(direct, signed);
    server.hide_from(bob.client.user_id(), direct);
    bob.client.full_sync().await.unwrap();

    assert_hidden_with_history(&bob, direct);
  }

  /// A DM on a device from before pins: the server leaves it out of a list, or answers 404, then calls
  /// it a channel. The record goes in between, the group doesn't, so the pin has to come first.
  #[tokio::test]
  async fn a_direct_message_left_out_and_then_relabelled_purges_nothing() {
    for is_by_gateway in [false, true] {
      let server = FakeServer::new();
      let (mut owner, mut bob) = two(&server).await;
      let mut admin = Peer::enroll(&server, "Admin").await;
      make_workspace_admin(&server, &mut owner, &mut admin).await;
      bob.client.full_sync().await.unwrap();
      let owner_id = owner.client.user_id();
      let bob_id = bob.client.user_id();
      let direct = bob.client.open_dm(owner_id).await.unwrap();
      bob.client.send_text(direct, None, "just us").await.unwrap();
      settle(&mut [&mut bob, &mut owner]).await;
      bob
        .store
        .delete(Table::AppliedChanges, &format!("direct/{direct}"))
        .unwrap();

      let membership = server.omit_from(bob_id, direct);
      if is_by_gateway {
        server.publish_event(
          &[bob_id],
          &GatewayEvent::ConversationUpdate {
            conversation_id: direct,
          },
        );
        settle(&mut [&mut bob]).await;
      } else {
        bob.client.full_sync().await.unwrap();
      }
      assert!(!is_cached(&bob, direct), "by gateway: {is_by_gateway}");
      assert!(bob.client.has_group(direct).unwrap());

      server.relabel_conversation(direct, ConversationKind::Channel);
      server.restore_for(bob_id, direct, membership);
      bob.client.full_sync().await.unwrap();
      let epoch = epoch_of(&mut bob, direct);
      let signed = admin.client.sign_channel_deletion(direct, epoch).unwrap();
      server.claim_signed_deletion(direct, signed);
      server.hide_from(bob_id, direct);
      bob.client.full_sync().await.unwrap();

      assert!(has_history(&bob, direct), "by gateway: {is_by_gateway}");
    }
  }

  /// Eve's demotion is withheld past the roster's age, so her own signature wouldn't do. The server
  /// dates back an entry of hers that makes Mallory, in the group, an admin: for roles it passes,
  /// but it anchors nothing, so Mallory's signature doesn't purge.
  #[tokio::test]
  async fn a_backdated_checkpoint_on_a_stale_roster_purges_nothing() {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut eve = Peer::enroll(&server, "Eve").await;
    let mut bob = Peer::enroll_on_clock(
      server.add_user("Bob"),
      "Bob",
      common::settings(),
      Box::new(clock.clone()),
    )
    .await;
    let mut mallory = Peer::enroll_on_clock(
      server.add_user("Mallory"),
      "Mallory",
      common::settings(),
      Box::new(clock.clone()),
    )
    .await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    make_workspace_admin(&server, &mut alice, &mut eve).await;
    add(&mut alice, &mut bob, &mut mallory, channel).await;
    bob.client.full_sync().await.unwrap();
    bob.client.sync_channel_roles(channel).await.unwrap();
    assert!(
      bob
        .client
        .channel_role_state(channel)
        .unwrap()
        .is_workspace_anchored
    );

    clock.advance(31 * 24 * 60 * 60 * 1000);
    let mallory_id = mallory.client.user_id();
    assert_eq!(
      eve
        .client
        .sign_channel_roles(channel, &[(mallory_id, ChannelRole::Admin)], false)
        .await
        .unwrap(),
      RoleSigning::Signed
    );
    bob.client.sync_channel_roles(channel).await.unwrap();
    assert_eq!(
      bob.client.verified_channel_role(channel, mallory_id).unwrap(),
      ChannelRole::Admin,
      "the entry passes for roles, at the time the server gives it"
    );

    let epoch = epoch_of(&mut bob, channel);
    let signed = mallory.client.sign_channel_deletion(channel, epoch).unwrap();
    server.claim_signed_deletion(channel, signed);
    server.hide_from(bob.client.user_id(), channel);
    bob.client.full_sync().await.unwrap();

    assert_hidden_with_history(&bob, channel);
  }

  /// Issue #1260: Bob reads the log after Eve left the roster. He takes Mallory's appointment for
  /// roles, but he can't justify the anchor any more: her deletion takes the retention.
  #[tokio::test]
  async fn a_channel_admin_appointed_by_a_departed_signer_purges_nothing_on_a_late_reader() {
    let server = FakeServer::new();
    let (mut alice, mut eve, mut bob, mallory, channel) = late_reader_world(&server).await;
    let mallory_id = mallory.client.user_id();

    assert_eq!(
      eve
        .client
        .sign_channel_roles(channel, &[(mallory_id, ChannelRole::Admin)], false)
        .await
        .unwrap(),
      RoleSigning::Signed
    );
    let alice_id = alice.client.user_id();
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    bob.client.sync_channel_roles(channel).await.unwrap();
    let roles = bob.client.channel_role_state(channel).unwrap();
    assert_eq!(roles.role_of(mallory_id), ChannelRole::Admin);
    assert!(!roles.is_workspace_anchored);

    let epoch = epoch_of(&mut bob, channel);
    let signed = mallory.client.sign_channel_deletion(channel, epoch).unwrap();
    server.claim_signed_deletion(channel, signed);
    server.hide_from(bob.client.user_id(), channel);
    bob.client.full_sync().await.unwrap();

    assert_hidden_with_history(&bob, channel);
  }

  /// Bob can't place Eve's entry at all. The channel's admins count for nothing on his device, but
  /// a workspace admin's signature doesn't rest on them: Alice's deletion still purges.
  #[tokio::test]
  async fn a_workspace_admin_still_purges_during_an_unverified_stretch() {
    let server = FakeServer::new();
    let (mut alice, mut eve, mut bob, mallory, channel) = late_reader_world(&server).await;
    let alice_id = alice.client.user_id();
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    bob.client.sync_workspace().await.unwrap();
    bob.client.sync_channel_roles(channel).await.unwrap();
    eve
      .client
      .sign_channel_roles(channel, &[(mallory.client.user_id(), ChannelRole::Admin)], false)
      .await
      .unwrap();
    bob.client.sync_channel_roles(channel).await.unwrap();
    assert!(
      bob
        .client
        .channel_role_state(channel)
        .unwrap()
        .has_unverified_stretch()
    );

    alice.client.delete_channel(channel).await.unwrap();
    bob.client.full_sync().await.unwrap();

    assert_purged(&bob, channel);
  }

  /// A device its user removed still has a good certificate; the user's signed list decides.
  #[tokio::test]
  async fn a_device_its_owner_removed_purges_nothing() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    let phone = common::link_device(&mut alice).await;
    let epoch = epoch_of(&mut bob, channel);
    let signed = phone.sign_channel_deletion(channel, epoch).unwrap();
    alice.client.revoke_device(phone.device_id()).await.unwrap();

    server.claim_signed_deletion(channel, signed);
    server.hide_from(bob.client.user_id(), channel);
    bob.client.full_sync().await.unwrap();

    assert_hidden_with_history(&bob, channel);
  }

  /// A mark a purge cut off doesn't outlive the server listing the conversation again.
  #[tokio::test]
  async fn being_listed_again_lifts_the_mark_of_a_purge_cut_off() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut alice, &mut bob).await;
    bob
      .store
      .put(Table::AppliedChanges, &format!("own_deletion/{channel}"), b"1")
      .unwrap();
    bob.client.full_sync().await.unwrap();

    server.hide_from(bob.client.user_id(), channel);
    bob.client.full_sync().await.unwrap();

    assert_hidden_with_history(&bob, channel);
  }

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

  /// `maybe_space_id` is the server's word, so a signature for "the channels of this space" could be
  /// had for any conversation: nothing is signed, also by a workspace admin.
  #[tokio::test]
  async fn it_is_never_signed_so_the_history_stays_for_the_retention() {
    let server = FakeServer::new();
    let (mut owner, mut bob) = two(&server).await;
    let channel = channel_with_history(&mut owner, &mut bob).await;
    let space = owner.space().await;
    server.set_admin(owner.client.user_id());

    owner.client.delete_space(space).await.unwrap();

    assert!(server.deletion_signature(channel).is_none());
    bob.client.full_sync().await.unwrap();
    assert_hidden_with_history(&bob, channel);
  }
}

/// Alice owns the workspace and Eve is an admin of it; Alice's channel holds Bob and Mallory, and
/// everybody's clock agrees with the server's, which dates the role entries.
async fn late_reader_world(server: &FakeServer) -> (Peer, Peer, Peer, Peer, ConversationId) {
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
  let mut alice = enroll("Alice").await;
  let mut eve = enroll("Eve").await;
  let mut bob = enroll("Bob").await;
  let mut mallory = enroll("Mallory").await;
  let channel = channel_with_history(&mut alice, &mut bob).await;
  make_workspace_admin(server, &mut alice, &mut eve).await;
  add(&mut alice, &mut bob, &mut mallory, channel).await;
  bob.client.full_sync().await.unwrap();
  bob.client.sync_channel_roles(channel).await.unwrap();
  assert!(
    bob
      .client
      .channel_role_state(channel)
      .unwrap()
      .is_workspace_anchored
  );
  eve.client.sync_channel_roles(channel).await.unwrap();
  (alice, eve, bob, mallory, channel)
}

async fn two(server: &FakeServer) -> (Peer, Peer) {
  (
    Peer::enroll(server, "Alice").await,
    Peer::enroll(server, "Bob").await,
  )
}

/// Alice's channel with Bob in it and a message both hold. She is the workspace's owner, so her
/// founding entry counts as a workspace admin's and the roles are anchored; Bob verified them.
async fn channel_with_history(alice: &mut Peer, bob: &mut Peer) -> ConversationId {
  let alice_id = alice.client.user_id();
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner)])
    .await
    .unwrap();
  bob.client.full_sync().await.unwrap();
  let channel = founder_rooted_channel(alice, bob).await;
  bob.client.sync_channel_roles(channel).await.unwrap();
  channel
}

/// [`channel_with_history`] without any roster: the roles go back to Alice's claim alone.
async fn founder_rooted_channel(alice: &mut Peer, bob: &mut Peer) -> ConversationId {
  let space = alice.space().await;
  let channel = alice.client.create_channel(space, "general").await.unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut *alice, &mut *bob]).await;
  bob.client.sync_channel_roles(channel).await.unwrap();
  alice.client.send_text(channel, None, "hello").await.unwrap();
  settle(&mut [&mut *alice, &mut *bob]).await;
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
