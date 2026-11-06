//! Signed channel roles (issue #380, e2ee.md §4.6.1) between clients and the fake server: the
//! creator founds the log, role changes are signed and verified by the other members, roles that
//! are the server's word only count for nothing, and a channel from before signed roles is founded
//! by the first app the server lists as its admin that sees it, naming who the server lists then.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::actions::NewChannel;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{AppointerAuthority, ChannelMember, RoleAppointer};
use teamcraft_core::api::fake::{FakeServer, WorkspaceLogFailure};
use teamcraft_core::client::RoleSigning;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::store::{LocalStore, Table};
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::{ChannelRole, ConversationId, SpaceId, UserId};

#[tokio::test]
async fn signed_role_changes_reach_the_members_and_server_only_roles_count_for_nothing() {
  let server = FakeServer::new();
  let (mut alice, mut bob, mut carol) = trio(&server).await;
  let channel = channel_with_everybody(&server, &mut alice, &mut [&mut bob, &mut carol]).await;
  let (alice_id, bob_id, carol_id) = ids(&alice, &bob, &carol);

  // The creator founded the log when she made the channel.
  carol.client.sync_channel_roles(channel).await.unwrap();
  assert_eq!(
    carol.client.verified_channel_role(channel, alice_id).unwrap(),
    ChannelRole::Admin
  );

  let signing = alice
    .client
    .set_channel_role(channel, bob_id, ChannelRole::Moderator)
    .await
    .unwrap();
  assert_eq!(signing, RoleSigning::Signed);
  carol.client.sync_channel_roles(channel).await.unwrap();
  assert_eq!(
    carol.client.verified_channel_role(channel, bob_id).unwrap(),
    ChannelRole::Moderator
  );

  // The server makes Carol an admin on its own: she shows as one, and counts as a member.
  server.set_channel_role_unsigned(channel, carol_id, ChannelRole::Admin);
  let bob_ui = refreshed_ui(&mut bob, channel).await;
  let carol_row = row(&bob_ui, channel, carol_id);
  assert_eq!(carol_row.role, ChannelRole::Admin);
  assert_eq!(
    carol_row.maybe_appointer, None,
    "nobody verified names an appointer"
  );
  // Alice founded the channel (trust on first use), so no appointer is claimed for Bob's role.
  assert_eq!(row(&bob_ui, channel, bob_id).maybe_appointer, None);
  assert_eq!(
    bob.client.verified_channel_role(channel, carol_id).unwrap(),
    ChannelRole::Member
  );
  // Carol's own device doesn't sign for her either: she isn't a signed admin.
  let own = carol
    .client
    .sign_channel_roles(channel, &[(bob_id, ChannelRole::Admin)], false)
    .await
    .unwrap();
  assert_eq!(own, RoleSigning::Unsigned);

  // Nothing signs what the server says by itself. A signed admin setting the role does; then it counts.
  let set = alice
    .client
    .set_channel_role(channel, carol_id, ChannelRole::Admin)
    .await
    .unwrap();
  assert_eq!(set, RoleSigning::Signed);
  let bob_ui = refreshed_ui(&mut bob, channel).await;
  assert_eq!(row(&bob_ui, channel, carol_id).maybe_appointer, None);
  assert_eq!(
    bob.client.verified_channel_role(channel, carol_id).unwrap(),
    ChannelRole::Admin
  );
}

#[tokio::test]
async fn entries_nobody_allowed_signed_are_skipped_without_blocking_the_log() {
  let server = FakeServer::new();
  let (mut alice, mut bob, mut carol) = trio(&server).await;
  let channel = channel_with_everybody(&server, &mut alice, &mut [&mut bob, &mut carol]).await;
  let (alice_id, bob_id, carol_id) = ids(&alice, &bob, &carol);
  carol.client.sync_channel_roles(channel).await.unwrap();

  // A hostile server (or a modified client) appends a promotion of Bob signed by nobody, and a
  // copy of the founding entry at a new position.
  let founding = server.role_log(channel).remove(0);
  let mut document: teamcraft_proto::ChannelRolesDocument =
    serde_json::from_slice(founding.payload.as_slice()).unwrap();
  document.version = 2;
  document.assignments.push(teamcraft_proto::ChannelRoleAssignment {
    user_id: bob_id,
    role: ChannelRole::Admin,
    identity_key: document.assignments[0].identity_key.clone(),
  });
  server.plant_role_log_entry(
    channel,
    serde_json::to_vec(&document).unwrap(),
    founding.signature.as_slice().to_vec(),
  );
  server.plant_role_log_entry(
    channel,
    founding.payload.as_slice().to_vec(),
    founding.signature.as_slice().to_vec(),
  );
  carol.client.sync_channel_roles(channel).await.unwrap();
  let state = carol.client.channel_role_state(channel).unwrap();
  assert_eq!((state.version, state.cursor), (1, 3));
  assert!(state.maybe_rejection.is_some());
  assert_eq!(
    carol.client.verified_channel_role(channel, bob_id).unwrap(),
    ChannelRole::Member
  );

  // Alice's next change builds on the last accepted entry and is taken.
  alice
    .client
    .set_channel_role(channel, carol_id, ChannelRole::Moderator)
    .await
    .unwrap();
  carol.client.sync_channel_roles(channel).await.unwrap();
  assert_eq!(
    carol.client.verified_channel_role(channel, carol_id).unwrap(),
    ChannelRole::Moderator
  );
  assert_eq!(
    carol.client.verified_channel_role(channel, alice_id).unwrap(),
    ChannelRole::Admin
  );
}

/// A channel from before signed roles, Alice its admin and Carol its moderator on the server's list.
async fn old_channel(
  server: &FakeServer,
  alice: &mut Peer,
  bob: &mut Peer,
  carol: &mut Peer,
) -> ConversationId {
  // An older server: the channel is made without a role log.
  server.set_role_log_lacking(true);
  let channel = channel_with_everybody(server, alice, &mut [bob, carol]).await;
  // Role changes keep working there, unsigned.
  let unsigned = alice
    .client
    .set_channel_role(channel, carol.client.user_id(), ChannelRole::Moderator)
    .await
    .unwrap();
  assert_eq!(unsigned, RoleSigning::Unsigned);
  server.set_role_log_lacking(false);
  settle(&mut [alice, bob, carol]).await;
  channel
}

mod founding_at_first_sight {
  use super::*;

  /// A change by the admin the server lists founds the log on the way, naming who the server lists.
  /// A member can't claim it.
  #[tokio::test]
  async fn the_first_change_by_a_listed_admin_founds_with_the_servers_list() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let channel = old_channel(&server, &mut alice, &mut bob, &mut carol).await;
    let (alice_id, bob_id, carol_id) = ids(&alice, &bob, &carol);
    assert!(!bob.client.channel_role_state(channel).unwrap().is_signed());

    // A member can't claim it: the server refuses, and nothing is signed.
    assert!(
      bob
        .client
        .set_channel_role(channel, bob_id, ChannelRole::Admin)
        .await
        .is_err()
    );
    assert_eq!(server.role_log(channel).len(), 0);

    let founded = alice
      .client
      .set_channel_role(channel, bob_id, ChannelRole::Moderator)
      .await
      .unwrap();
    assert_eq!(founded, RoleSigning::Signed);
    bob.client.sync_channel_roles(channel).await.unwrap();
    let state = bob.client.channel_role_state(channel).unwrap();
    assert_eq!(state.role_of(alice_id), ChannelRole::Admin);
    assert_eq!(state.role_of(bob_id), ChannelRole::Moderator);
    assert_eq!(
      state.role_of(carol_id),
      ChannelRole::Moderator,
      "she was on the list"
    );
    assert!(state.has_server_listed_roles);
    assert!(!state.is_workspace_anchored);
  }

  /// Nobody acts at all: the admin's app founds in its upkeep, once, and a member's does nothing.
  #[tokio::test]
  async fn the_upkeep_of_the_listed_admin_founds_once_and_a_members_never() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let channel = old_channel(&server, &mut alice, &mut bob, &mut carol).await;
    let (alice_id, _, carol_id) = ids(&alice, &bob, &carol);

    bob.client.found_listed_channels().await.unwrap();
    carol.client.found_listed_channels().await.unwrap();
    assert_eq!(server.role_log(channel).len(), 0, "neither is its admin");

    alice.client.found_listed_channels().await.unwrap();
    assert_eq!(server.role_log(channel).len(), 1);
    alice.client.found_listed_channels().await.unwrap();
    assert_eq!(server.role_log(channel).len(), 1);

    carol.client.sync_channel_roles(channel).await.unwrap();
    assert_eq!(
      carol.client.verified_channel_role(channel, alice_id).unwrap(),
      ChannelRole::Admin
    );
    assert_eq!(
      carol.client.verified_channel_role(channel, carol_id).unwrap(),
      ChannelRole::Moderator
    );
  }

  /// A second admin the server lists is named too, and needs nothing further: the role counts.
  #[tokio::test]
  async fn a_co_owner_on_the_list_counts_without_anybody_doing_anything() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let channel = old_channel(&server, &mut alice, &mut bob, &mut carol).await;
    let bob_id = bob.client.user_id();
    server.set_channel_role_unsigned(channel, bob_id, ChannelRole::Admin);

    alice.client.found_listed_channels().await.unwrap();
    bob.client.full_sync().await.unwrap();
    // Bob's own upkeep finds a log and adds nothing to it.
    bob.client.found_listed_channels().await.unwrap();
    assert_eq!(server.role_log(channel).len(), 1);
    assert_eq!(
      bob.client.channel_role_state(channel).unwrap().role_of(bob_id),
      ChannelRole::Admin
    );
  }

  /// What a hostile server can get signed. An account it made up, or anybody the group doesn't hold,
  /// isn't named. A member of the group is, but that gives no anchor (e2ee.md §4.6.1).
  #[tokio::test]
  async fn the_server_names_only_people_in_the_group_and_gains_no_anchor() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let channel = old_channel(&server, &mut alice, &mut bob, &mut carol).await;
    let (alice_id, bob_id, _) = ids(&alice, &bob, &carol);
    let mallory = Peer::enroll(&server, "Mallory").await;
    let mallory_id = mallory.client.user_id();
    server.list_member_unsigned(channel, mallory_id, ChannelRole::Admin);
    server.set_channel_role_unsigned(channel, bob_id, ChannelRole::Admin);

    alice
      .client
      .set_channel_role(channel, bob_id, ChannelRole::Admin)
      .await
      .unwrap();

    let state = alice.client.channel_role_state(channel).unwrap();
    assert_eq!(state.role_of(mallory_id), ChannelRole::Member);
    assert_eq!(state.role_of(bob_id), ChannelRole::Admin);
    assert_eq!(state.role_of(alice_id), ChannelRole::Admin);
    assert!(!state.is_workspace_anchored);
  }

  /// The server can't get a list signed over roles a device has, by pretending it is first sight or
  /// by showing the log empty: nobody is stripped, and nothing new is signed.
  #[tokio::test]
  async fn a_device_with_a_log_founds_nothing_whatever_the_server_says() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let channel = channel_with_everybody(&server, &mut alice, &mut [&mut bob, &mut carol]).await;
    let (alice_id, bob_id, carol_id) = ids(&alice, &bob, &carol);
    alice
      .client
      .set_channel_role(channel, bob_id, ChannelRole::Admin)
      .await
      .unwrap();
    bob.client.sync_channel_roles(channel).await.unwrap();
    let before = server.role_log(channel).len();

    // The server takes Alice's role away, gives Carol one, and asks Bob's app what it does.
    server.set_channel_role_unsigned(channel, alice_id, ChannelRole::Member);
    server.set_channel_role_unsigned(channel, carol_id, ChannelRole::Admin);
    bob.client.full_sync().await.unwrap();
    bob.client.found_listed_channels().await.unwrap();

    assert_eq!(server.role_log(channel).len(), before);
    let state = bob.client.channel_role_state(channel).unwrap();
    assert_eq!(state.role_of(alice_id), ChannelRole::Admin, "not stripped");
    assert_eq!(state.role_of(carol_id), ChannelRole::Member, "not appointed");
  }

  /// A workspace admin who founds or changes a role there vouches for nothing they didn't see. Only
  /// the list they looked at anchors the channel.
  #[tokio::test]
  async fn a_workspace_admins_founding_anchors_nothing_until_they_looked() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let (alice_id, bob_id, carol_id) = ids(&alice, &bob, &carol);
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();
    carol.client.full_sync().await.unwrap();
    let channel = old_channel(&server, &mut alice, &mut bob, &mut carol).await;

    alice
      .client
      .set_channel_role(channel, bob_id, ChannelRole::Moderator)
      .await
      .unwrap();
    alice
      .client
      .set_channel_role(channel, bob_id, ChannelRole::Admin)
      .await
      .unwrap();
    carol.client.sync_channel_roles(channel).await.unwrap();
    let state = carol.client.channel_role_state(channel).unwrap();
    assert_eq!(state.role_of(bob_id), ChannelRole::Admin);
    assert!(!state.is_workspace_anchored);
    assert_eq!(state.appointment_of(bob_id), None);

    let at_cursor = alice.client.channel_role_state(channel).unwrap().cursor;
    let looked_at = [
      (alice_id, ChannelRole::Admin),
      (bob_id, ChannelRole::Admin),
      (carol_id, ChannelRole::Moderator),
    ];
    alice
      .client
      .sign_reviewed_channel_roles(channel, &looked_at, at_cursor)
      .await
      .unwrap();
    carol.client.sync_channel_roles(channel).await.unwrap();
    assert!(
      carol
        .client
        .channel_role_state(channel)
        .unwrap()
        .is_workspace_anchored
    );
  }

  /// A workspace admin outside the group can't check the list: their change stands on the server,
  /// and the channel's own admin founds it, with that change on the list.
  #[tokio::test]
  async fn a_workspace_admin_outside_the_group_leaves_founding_to_the_channels_admin() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let (alice_id, bob_id, _) = ids(&alice, &bob, &carol);
    let mut owner = Peer::enroll(&server, "Olivia").await;
    let owner_id = owner.client.user_id();
    owner
      .client
      .publish_roster(&[(owner_id, RosterRole::Owner)])
      .await
      .unwrap();
    server.set_admin(owner_id);
    let channel = old_channel(&server, &mut alice, &mut bob, &mut carol).await;
    owner.client.full_sync().await.unwrap();

    let signing = owner
      .client
      .set_channel_role(channel, bob_id, ChannelRole::Moderator)
      .await
      .unwrap();
    assert_eq!(signing, RoleSigning::Unsigned);
    assert_eq!(server.role_log(channel).len(), 0);

    alice.client.found_listed_channels().await.unwrap();
    let state = alice.client.channel_role_state(channel).unwrap();
    assert_eq!(state.role_of(alice_id), ChannelRole::Admin);
    assert_eq!(state.role_of(bob_id), ChannelRole::Moderator);
  }

  /// The channel stays usable while founding fails, and it is tried again, not every pass.
  #[tokio::test]
  async fn a_failed_founding_waits_and_is_tried_again() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let channel = old_channel(&server, &mut alice, &mut bob, &mut carol).await;
    server.fail_role_log(channel, true);
    alice.client.found_listed_channels().await.unwrap();
    assert_eq!(server.role_log(channel).len(), 0);
    let reads = server.role_log_reads(channel);
    alice.client.found_listed_channels().await.unwrap();
    assert_eq!(server.role_log_reads(channel), reads, "not again at once");
    server.fail_role_log(channel, false);
    assert!(!alice.client.channel_role_state(channel).unwrap().is_signed());
  }

  /// The panel is there for a workspace admin where the list shows a role nobody confirmed, and for
  /// nobody else; a member gets no line of any kind.
  #[tokio::test]
  async fn only_a_workspace_admin_gets_the_look_at_a_role_nobody_confirmed() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let (alice_id, bob_id, carol_id) = ids(&alice, &bob, &carol);
    let mut owner = Peer::enroll(&server, "Olivia").await;
    let owner_id = owner.client.user_id();
    owner
      .client
      .publish_roster(&[(owner_id, RosterRole::Owner)])
      .await
      .unwrap();
    server.set_admin(owner_id);
    for peer in [&mut alice, &mut bob, &mut carol] {
      peer.client.full_sync().await.unwrap();
    }
    let channel = channel_with_everybody(&server, &mut alice, &mut [&mut bob, &mut carol]).await;
    let ui = backend_of(&owner);
    ui.join_channel(channel);
    run_commands(&mut owner, &ui).await;
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut owner]).await;

    let owner_ui = refreshed_ui(&mut owner, channel).await;
    assert!(
      !owner_ui.channel_roles_review(channel).unwrap().needs_a_look,
      "there for them, with nothing to say"
    );

    server.set_channel_role_unsigned(channel, carol_id, ChannelRole::Admin);
    let owner_ui = refreshed_ui(&mut owner, channel).await;
    assert!(owner_ui.channel_roles_review(channel).unwrap().needs_a_look);
    let alice_ui = refreshed_ui(&mut alice, channel).await;
    assert!(alice_ui.channel_roles_review(channel).is_none());
    let bob_ui = refreshed_ui(&mut bob, channel).await;
    assert!(bob_ui.channel_roles_review(channel).is_none());
    let _ = (alice_id, bob_id);
  }
}

/// The review of #1286: what a workspace admin's signature may say about somebody's key.
mod whose_key_a_role_names {
  use super::*;

  /// Finding 1: a workspace admin outside the group is about to confirm a list with somebody they
  /// never met. Their first pin would be whatever the server says, and that key anchored would be
  /// a channel admin of the server's own. Nothing is signed, and the panel says so.
  #[tokio::test]
  async fn an_outside_workspace_admin_confirms_no_key_it_can_t_check() {
    let server = FakeServer::new();
    let world = anchored_with_outside_owner(&server).await;
    let (mut alice, mut bob, mut carol, mut owner, channel) = world;
    let carol_id = carol.client.user_id();
    let before = server.role_log(channel).len();
    // The server lists Carol, whom the snapshot doesn't name and this device never pinned.
    server.set_channel_role_unsigned(channel, carol_id, ChannelRole::Admin);

    let ui = refreshed_ui(&mut owner, channel).await;
    let review = ui.channel_roles_review(channel).unwrap();
    assert!(
      review
        .key_marks
        .contains(&(carol_id, teamcraft_client_api::model::RoleKeyMark::Uncheckable)),
      "{:?}",
      review.key_marks
    );
    let roles = [
      (alice.client.user_id(), ChannelRole::Admin),
      (carol_id, ChannelRole::Admin),
    ];
    let signing = owner
      .client
      .sign_reviewed_channel_roles(channel, &roles, review.log_cursor)
      .await
      .unwrap();
    assert_eq!(signing, RoleSigning::Unsigned);
    assert_eq!(server.role_log(channel).len(), before);
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut owner]).await;
  }

  /// Finding 1, the swap: the server puts its own key in place of Alice's. A workspace admin outside
  /// the group confirms the list, and the key signed stays the one the anchored snapshot names, and
  /// the row says this device saw another. Everybody keeps verifying the real Alice.
  #[tokio::test]
  async fn a_swapped_key_is_marked_and_never_signed() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol, mut owner, channel) = anchored_with_outside_owner(&server).await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    let real_key = owner
      .client
      .channel_role_state(channel)
      .unwrap()
      .snapshot_key_of(bob_id)
      .unwrap();

    // What a first pin from the server looks like: the server's key, unattested.
    server.replace_identity_key(bob_id, vec![9; 32]);
    teamcraft_core::records::pin_identity(
      &owner.store,
      bob_id,
      &teamcraft_core::records::PinnedIdentity {
        public_key: [9; 32],
        maybe_changed_to: None,
        is_attested: false,
        maybe_attestation_checked_at_ms: None,
      },
    )
    .unwrap();
    server.set_channel_role_unsigned(channel, bob_id, ChannelRole::Admin);

    let ui = refreshed_ui(&mut owner, channel).await;
    let review = ui.channel_roles_review(channel).unwrap();
    assert!(
      review
        .key_marks
        .contains(&(bob_id, teamcraft_client_api::model::RoleKeyMark::Changed)),
      "{:?}",
      review.key_marks
    );

    let roles = [(alice_id, ChannelRole::Admin), (bob_id, ChannelRole::Admin)];
    assert_eq!(
      owner
        .client
        .sign_reviewed_channel_roles(channel, &roles, review.log_cursor)
        .await
        .unwrap(),
      RoleSigning::Signed
    );
    carol.client.sync_channel_roles(channel).await.unwrap();
    let state = carol.client.channel_role_state(channel).unwrap();
    assert_eq!(state.role_of(bob_id), ChannelRole::Admin);
    assert_eq!(
      state.snapshot_key_of(bob_id),
      Some(real_key),
      "the server's key is in nobody's snapshot"
    );
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut owner]).await;
  }

  /// Third review: a colluding member founds a channel from before signed roles, unmarked, with the
  /// server listing them. A workspace admin in the group makes one ordinary change, and every key
  /// checks. That counts for roles, but it anchors nothing: Mallory, in the group, can't purge at once.
  #[tokio::test]
  async fn a_workspace_admins_ordinary_change_anchors_no_colluding_founder() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let (alice_id, bob_id, carol_id) = ids(&alice, &bob, &carol);
    let mut owner = outside_owner(&server).await;
    let channel = old_channel(&server, &mut alice, &mut bob, &mut carol).await;
    let ui = backend_of(&owner);
    ui.join_channel(channel);
    run_commands(&mut owner, &ui).await;
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut owner]).await;

    // Bob, the colluding member: the server lists him, and he founds it by hand, without the mark.
    server.set_channel_role_unsigned(channel, bob_id, ChannelRole::Admin);
    assert_eq!(
      bob
        .client
        .sign_channel_roles(channel, &[(bob_id, ChannelRole::Admin)], true)
        .await
        .unwrap(),
      RoleSigning::Signed
    );
    assert_eq!(
      owner
        .client
        .sign_channel_roles(channel, &[(carol_id, ChannelRole::Moderator)], false)
        .await
        .unwrap(),
      RoleSigning::Signed,
      "every key checks"
    );

    alice.client.sync_channel_roles(channel).await.unwrap();
    let state = alice.client.channel_role_state(channel).unwrap();
    assert_eq!(state.role_of(bob_id), ChannelRole::Admin);
    assert_eq!(state.role_of(alice_id), ChannelRole::Member);
    assert!(!state.is_workspace_anchored, "nothing Bob can purge at once with");
    assert_eq!(state.appointment_of(carol_id), None);
  }

  /// Follow-up review: the server forges a founding that names its key for Alice, and an outside
  /// workspace admin's device takes it, pinning that key on the way. Ordinary change or confirm, it
  /// signs nothing: it can't check a key on a snapshot nobody anchored. Bob, who has Alice's real pin,
  /// never ends up anchored on the server's key.
  #[tokio::test]
  async fn a_forged_founding_gets_no_signature_from_an_outside_workspace_admin() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let (alice_id, _, carol_id) = ids(&alice, &bob, &carol);
    let mut owner = outside_owner(&server).await;
    let channel = old_channel(&server, &mut alice, &mut bob, &mut carol).await;
    bob.client.full_sync().await.unwrap();

    let forged_key = plant_forged_founding(&server, channel, alice_id);
    server.replace_identity_key(alice_id, forged_key.to_vec());

    owner.client.full_sync().await.unwrap();
    owner.client.sync_channel_roles(channel).await.unwrap();
    let state = owner.client.channel_role_state(channel).unwrap();
    assert_eq!(state.version, 1, "the owner's device took it");
    assert_eq!(state.snapshot_key_of(alice_id), Some(forged_key));
    assert!(!state.is_workspace_anchored);

    let before = server.role_log(channel).len();
    assert_eq!(
      owner
        .client
        .set_channel_role(channel, carol_id, ChannelRole::Moderator)
        .await
        .unwrap(),
      RoleSigning::Unsigned,
      "no ordinary entry"
    );
    assert_eq!(
      owner
        .client
        .sign_channel_roles(channel, &[(alice_id, ChannelRole::Admin)], false)
        .await
        .unwrap(),
      RoleSigning::Unsigned,
      "nor on the person it can't check"
    );
    let roles = [(alice_id, ChannelRole::Admin), (carol_id, ChannelRole::Moderator)];
    assert_eq!(
      owner
        .client
        .sign_reviewed_channel_roles(channel, &roles, state.cursor)
        .await
        .unwrap(),
      RoleSigning::Unsigned,
      "no confirm"
    );
    assert_eq!(server.role_log(channel).len(), before, "nothing signed at all");

    let ui = refreshed_ui(&mut owner, channel).await;
    let review = ui.channel_roles_review(channel).unwrap();
    assert!(
      review
        .key_marks
        .contains(&(alice_id, teamcraft_client_api::model::RoleKeyMark::Uncheckable)),
      "the panel says so: {:?}",
      review.key_marks
    );

    bob.client.sync_channel_roles(channel).await.unwrap();
    let state = bob.client.channel_role_state(channel).unwrap();
    assert!(!state.is_workspace_anchored);
    assert_ne!(state.snapshot_key_of(alice_id), Some(forged_key));
  }

  /// The same state by a neighbouring route: a workspace admin in the group finds a forged key on a
  /// snapshot. Their device knows the real one from the group, so it won't repeat the forged one in an
  /// ordinary entry; the list they confirm names the real one, marked.
  #[tokio::test]
  async fn a_workspace_admin_in_the_group_repeats_no_forged_key() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let (alice_id, bob_id, carol_id) = ids(&alice, &bob, &carol);
    let mut owner = outside_owner(&server).await;
    let channel = old_channel(&server, &mut alice, &mut bob, &mut carol).await;
    let ui = backend_of(&owner);
    ui.join_channel(channel);
    run_commands(&mut owner, &ui).await;
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut owner]).await;

    // In Bob's name, naming Alice under the server's key. This device took Bob's pin from the server
    // too, the one way it takes the founding at all; Alice's it holds from the group.
    let forged_key = plant_forged_founding_by(&server, channel, bob_id, alice_id);
    forge_pin_of_founder(&owner, &server, bob_id, forged_key);
    owner.client.sync_channel_roles(channel).await.unwrap();
    let state = owner.client.channel_role_state(channel).unwrap();
    assert_eq!(state.version, 1);
    assert_eq!(state.snapshot_key_of(alice_id), Some(forged_key));
    assert!(!state.is_workspace_anchored);

    let before = server.role_log(channel).len();
    assert_eq!(
      owner
        .client
        .sign_channel_roles(channel, &[(carol_id, ChannelRole::Moderator)], false)
        .await
        .unwrap(),
      RoleSigning::Unsigned,
      "its own group says Alice's key is another"
    );
    assert_eq!(server.role_log(channel).len(), before);
  }

  /// An older workspace admin's client still signs a first pin. A device with the real pin takes the
  /// role, as it can't tell, but no anchor on a key it knows of another for.
  #[tokio::test]
  async fn a_device_with_the_real_pin_takes_no_anchor_on_another_key() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol, mut owner, channel) = anchored_with_outside_owner(&server).await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    carol.client.sync_channel_roles(channel).await.unwrap();
    assert!(
      carol
        .client
        .channel_role_state(channel)
        .unwrap()
        .is_workspace_anchored
    );

    // What an older client signs: its snapshot, with the server's key for Bob in it.
    forget_roles_on(&owner, channel, |state| {
      for assignment in state["assignments"].as_array_mut().unwrap() {
        if assignment["user_id"] == serde_json::json!(bob_id.to_string()) {
          assignment["identity_key"] =
            serde_json::to_value(teamcraft_proto::Base64Bytes::new(vec![9; 32])).unwrap();
        }
      }
    });
    assert_eq!(
      owner
        .client
        .sign_channel_roles(channel, &[(alice_id, ChannelRole::Admin)], false)
        .await
        .unwrap(),
      RoleSigning::Signed
    );

    carol.client.sync_channel_roles(channel).await.unwrap();
    let state = carol.client.channel_role_state(channel).unwrap();
    assert_eq!(
      state.snapshot_key_of(bob_id),
      Some([9; 32]),
      "it can't refuse the role"
    );
    assert!(
      !state.is_workspace_anchored,
      "but no anchor on a key it knows of another for"
    );
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut owner]).await;
  }

  /// Finding 1, the same root: a workspace admin's ordinary role change on somebody they can't check
  /// stays the server's, and they are told, in the app's command.
  #[tokio::test]
  async fn an_outside_workspace_admin_s_change_on_somebody_unchecked_is_not_signed_and_says_so() {
    let server = FakeServer::new();
    let (_alice, _bob, carol, mut owner, channel) = anchored_with_outside_owner(&server).await;
    let carol_id = carol.client.user_id();
    let before = server.role_log(channel).len();
    let signing = owner
      .client
      .set_channel_role(channel, carol_id, ChannelRole::Moderator)
      .await
      .unwrap();
    assert_eq!(signing, RoleSigning::Unsigned);
    assert_eq!(server.role_log(channel).len(), before);
    assert!(
      owner.client.could_sign_channel_roles(channel).unwrap(),
      "so the app's command reports it (`RoleChangeNotConfirmed`)"
    );
  }

  /// A workspace admin in the group checks against it: the same change signs.
  #[tokio::test]
  async fn a_workspace_admin_in_the_group_signs_who_the_group_holds() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let (alice_id, _, carol_id) = ids(&alice, &bob, &carol);
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();
    carol.client.full_sync().await.unwrap();
    let channel = channel_with_everybody(&server, &mut alice, &mut [&mut bob, &mut carol]).await;
    assert_eq!(
      alice
        .client
        .set_channel_role(channel, carol_id, ChannelRole::Moderator)
        .await
        .unwrap(),
      RoleSigning::Signed
    );
  }

  /// Finding 2: one junk entry used to get an unmarked entry out of a workspace admin. Nothing is
  /// signed on a log this device took nothing from, other than a list they looked at.
  #[tokio::test]
  async fn a_junk_entry_gets_no_workspace_admin_s_signature() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let (alice_id, bob_id, carol_id) = ids(&alice, &bob, &carol);
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();
    carol.client.full_sync().await.unwrap();
    let channel = old_channel(&server, &mut alice, &mut bob, &mut carol).await;
    server.plant_role_log_entry(channel, b"junk".to_vec(), vec![0; 64]);

    let signing = alice
      .client
      .set_channel_role(channel, bob_id, ChannelRole::Admin)
      .await
      .unwrap();
    assert_eq!(signing, RoleSigning::Unsigned);
    assert_eq!(server.role_log(channel).len(), 1, "only the junk");
    alice.client.found_listed_channels().await.unwrap();
    assert_eq!(server.role_log(channel).len(), 1, "no founding either");

    // Only a list she looked at gets it out.
    let ui = refreshed_ui(&mut alice, channel).await;
    let review = ui.channel_roles_review(channel).unwrap();
    let roles = [(alice_id, ChannelRole::Admin), (carol_id, ChannelRole::Moderator)];
    assert_eq!(
      alice
        .client
        .sign_reviewed_channel_roles(channel, &roles, review.log_cursor)
        .await
        .unwrap(),
      RoleSigning::Signed
    );
    bob.client.sync_channel_roles(channel).await.unwrap();
    let state = bob.client.channel_role_state(channel).unwrap();
    assert_eq!(state.role_of(alice_id), ChannelRole::Admin);
    assert!(state.is_workspace_anchored);
  }

  /// Finding 2: an ordinary workspace admin's entry on another head (their view is what a junk entry
  /// or a fork made it) strips nobody on a device with a log. Only a reviewed list is a checkpoint.
  #[tokio::test]
  async fn an_ordinary_entry_on_another_head_replaces_nothing() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let (alice_id, bob_id, carol_id) = ids(&alice, &bob, &carol);
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();
    carol.client.full_sync().await.unwrap();
    let channel = channel_with_everybody(&server, &mut alice, &mut [&mut bob, &mut carol]).await;
    alice
      .client
      .set_channel_role(channel, bob_id, ChannelRole::Admin)
      .await
      .unwrap();
    bob.client.sync_channel_roles(channel).await.unwrap();
    let before = server.role_log(channel).len();

    // Alice's device is on a head of its own, as a junk entry or a fork could have made it.
    forget_roles_on(&alice, channel, |state| {
      state["head_hash"] = serde_json::json!("AQID");
    });
    assert_eq!(
      alice
        .client
        .sign_channel_roles(channel, &[(carol_id, ChannelRole::Admin)], false)
        .await
        .unwrap(),
      RoleSigning::Signed
    );
    assert_eq!(server.role_log(channel).len(), before + 1);

    bob.client.sync_channel_roles(channel).await.unwrap();
    let state = bob.client.channel_role_state(channel).unwrap();
    assert_eq!(state.role_of(alice_id), ChannelRole::Admin, "not stripped");
    assert_eq!(state.role_of(bob_id), ChannelRole::Admin, "not stripped");
    assert_eq!(state.role_of(carol_id), ChannelRole::Member, "not taken");
  }

  /// Finding 3: an older client's ordinary entry doesn't carry the mark on. A device that knows the
  /// mark keeps it and doesn't anchor: only a reviewed list ends it.
  #[tokio::test]
  async fn an_older_clients_entry_doesn_t_end_the_mark() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let (alice_id, bob_id, carol_id) = ids(&alice, &bob, &carol);
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();
    carol.client.full_sync().await.unwrap();
    let channel = old_channel(&server, &mut alice, &mut bob, &mut carol).await;
    alice.client.found_listed_channels().await.unwrap();
    bob.client.sync_channel_roles(channel).await.unwrap();
    assert!(
      bob
        .client
        .channel_role_state(channel)
        .unwrap()
        .has_server_listed_roles
    );

    // What an older client's device would sign: its state has no idea of the mark.
    forget_roles_on(&alice, channel, |state| {
      state.as_object_mut().unwrap().remove("has_server_listed_roles");
    });
    assert_eq!(
      alice
        .client
        .sign_channel_roles(channel, &[(bob_id, ChannelRole::Moderator)], false)
        .await
        .unwrap(),
      RoleSigning::Signed
    );
    let document: teamcraft_proto::ChannelRolesDocument =
      serde_json::from_slice(server.role_log(channel).last().unwrap().payload.as_slice()).unwrap();
    assert!(!document.has_server_listed_roles, "no mark on this entry");

    bob.client.sync_channel_roles(channel).await.unwrap();
    let state = bob.client.channel_role_state(channel).unwrap();
    assert_eq!(
      state.role_of(bob_id),
      ChannelRole::Moderator,
      "it counts for the role"
    );
    assert!(state.has_server_listed_roles, "the mark stays");
    assert!(!state.is_workspace_anchored, "no anchor");
    assert_eq!(state.role_of(carol_id), ChannelRole::Moderator);
  }

  /// Finding 4: a contradiction reaches this device's person, and a workspace admin who can look.
  #[tokio::test]
  async fn an_alarm_reaches_the_app_and_a_reviewed_list_ends_it() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = trio(&server).await;
    let (alice_id, bob_id, _) = ids(&alice, &bob, &carol);
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();
    carol.client.full_sync().await.unwrap();
    let channel = channel_with_everybody(&server, &mut alice, &mut [&mut bob, &mut carol]).await;
    alice.client.sync_channel_roles(channel).await.unwrap();
    bob.client.sync_channel_roles(channel).await.unwrap();
    let alarm = serde_json::json!("another member verified a different entry");
    forget_roles_on(&bob, channel, |state| state["alarm"] = alarm.clone());
    forget_roles_on(&alice, channel, |state| state["alarm"] = alarm.clone());
    // The state's writer keeps the list the UI reads: the next read writes it.
    bob.client.sync_channel_roles(channel).await.unwrap();
    alice.client.sync_channel_roles(channel).await.unwrap();

    let bob_ui = refreshed_ui(&mut bob, channel).await;
    assert_eq!(bob_ui.channel_role_alarms(), vec![channel]);
    assert!(
      bob_ui.channel_roles_review(channel).is_none(),
      "he can't look, only tell"
    );
    let alice_ui = refreshed_ui(&mut alice, channel).await;
    let review = alice_ui.channel_roles_review(channel).expect("she can look");

    let roles = [(alice_id, ChannelRole::Admin)];
    alice
      .client
      .sign_reviewed_channel_roles(channel, &roles, review.log_cursor)
      .await
      .unwrap();
    bob.client.sync_channel_roles(channel).await.unwrap();
    assert!(backend_of(&alice).channel_role_alarms().is_empty());
    assert!(backend_of(&bob).channel_role_alarms().is_empty());
    assert_eq!(
      bob.client.channel_role_state(channel).unwrap().role_of(bob_id),
      ChannelRole::Member
    );
  }

  /// A channel of Alice, a workspace admin, with Bob and Carol in it: its log is anchored and names
  /// Alice and Bob. Olivia owns the workspace and isn't in the channel's group; she has read the log.
  async fn anchored_with_outside_owner(server: &FakeServer) -> (Peer, Peer, Peer, Peer, ConversationId) {
    let (mut alice, mut bob, mut carol) = trio(server).await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    let mut owner = outside_owner(server).await;
    let owner_id = owner.client.user_id();
    owner
      .client
      .publish_roster(&[(owner_id, RosterRole::Owner), (alice_id, RosterRole::Admin)])
      .await
      .unwrap();
    for peer in [&mut alice, &mut bob, &mut carol] {
      peer.client.full_sync().await.unwrap();
    }
    let channel = channel_with_everybody(server, &mut alice, &mut [&mut bob, &mut carol]).await;
    alice
      .client
      .set_channel_role(channel, bob_id, ChannelRole::Moderator)
      .await
      .unwrap();
    owner.client.sync_channel_roles(channel).await.unwrap();
    let state = owner.client.channel_role_state(channel).unwrap();
    assert!(state.is_workspace_anchored);
    assert_eq!(state.role_of(bob_id), ChannelRole::Moderator);
    (alice, bob, carol, owner, channel)
  }

  /// Plants version 1 of the channel's log: signed by a device of `alice` under a key the server made
  /// up, naming that key for her. Returns the key.
  fn plant_forged_founding(server: &FakeServer, channel: ConversationId, alice: UserId) -> [u8; 32] {
    plant_forged_founding_by(server, channel, alice, alice)
  }

  /// As [`plant_forged_founding`], signed in the name of `signer` and naming them admin too.
  fn plant_forged_founding_by(
    server: &FakeServer,
    channel: ConversationId,
    signer: UserId,
    named: UserId,
  ) -> [u8; 32] {
    use rand_core::OsRng;
    use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair};
    use teamcraft_crypto::workspace::{WorkspaceLog, sign_log_entry};
    use teamcraft_proto::{Base64Bytes, ChannelRoleAssignment, ChannelRolesDocument, Id};

    let crypto_user =
      |user: UserId| teamcraft_crypto::ids::UserId::new(u64::try_from(user.snowflake().get()).unwrap());
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let key = *identity.public_key().as_bytes();
    let device = DeviceKeyPair::generate(
      crypto_user(signer),
      teamcraft_crypto::ids::DeviceId::new(77),
      &mut OsRng,
    );
    let certificate = identity
      .certify_device(crypto_user(signer), &device.public_keys())
      .unwrap();
    let mut assignments = vec![ChannelRoleAssignment {
      user_id: signer,
      role: ChannelRole::Admin,
      identity_key: Base64Bytes::new(key.to_vec()),
    }];
    if named != signer {
      assignments.push(ChannelRoleAssignment {
        user_id: named,
        role: ChannelRole::Admin,
        identity_key: Base64Bytes::new(key.to_vec()),
      });
    }
    assignments.sort_by_key(|assignment| assignment.user_id);
    let document = ChannelRolesDocument {
      version: 1,
      conversation_id: channel,
      assignments,
      prev_hash: Base64Bytes::new(vec![0; 32]),
      signed_at_ms: server.server_now_ms(),
      has_server_listed_roles: false,
    };
    let payload = serde_json::to_vec(&document).unwrap();
    let signature = sign_log_entry(&device, &certificate, WorkspaceLog::ChannelRoles, 1, &payload).unwrap();
    server.plant_role_log_entry(channel, payload, signature);
    key
  }

  /// The device took `signer`'s key as the forged one, as a first pin from the server would.
  fn forge_pin_of_founder(peer: &Peer, server: &FakeServer, signer: UserId, key: [u8; 32]) {
    server.replace_identity_key(signer, key.to_vec());
    teamcraft_core::records::pin_identity(
      &peer.store,
      signer,
      &teamcraft_core::records::PinnedIdentity {
        public_key: key,
        maybe_changed_to: None,
        is_attested: false,
        maybe_attestation_checked_at_ms: None,
      },
    )
    .unwrap();
  }

  async fn outside_owner(server: &FakeServer) -> Peer {
    let mut owner = Peer::enroll(server, "Olivia").await;
    let owner_id = owner.client.user_id();
    owner
      .client
      .publish_roster(&[(owner_id, RosterRole::Owner)])
      .await
      .unwrap();
    server.set_admin(owner_id);
    owner
  }

  /// Edits this device's stored role state, as a device in that state would have it.
  fn forget_roles_on(peer: &Peer, channel: ConversationId, edit: impl FnOnce(&mut serde_json::Value)) {
    let key = format!("channel_roles/{channel}");
    let stored = peer.store.get(Table::Workspace, &key).unwrap().unwrap();
    let mut state: serde_json::Value = serde_json::from_slice(&stored).unwrap();
    edit(&mut state);
    peer
      .store
      .put(Table::Workspace, &key, &serde_json::to_vec(&state).unwrap())
      .unwrap();
  }
}

/// A workspace admin of the signed roster appoints Bob: Alice's device, which verified the entry,
/// names the admin and the authority (the name is the member directory's).
#[tokio::test]
async fn a_workspace_admins_appointment_is_shown_with_its_signer_and_authority() {
  let server = FakeServer::new();
  let (mut alice, mut bob, mut carol) = trio(&server).await;
  let (alice_id, bob_id, carol_id) = ids(&alice, &bob, &carol);
  // Alice owns the workspace and Carol is one of its admins, both on the signed roster.
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (carol_id, RosterRole::Admin)])
    .await
    .unwrap();
  server.set_admin(carol_id);
  for peer in [&mut bob, &mut carol] {
    peer.client.full_sync().await.unwrap();
  }
  let channel = channel_with_everybody(&server, &mut alice, &mut [&mut bob, &mut carol]).await;

  let signing = carol
    .client
    .set_channel_role(channel, bob_id, ChannelRole::Admin)
    .await
    .unwrap();
  assert_eq!(signing, RoleSigning::Signed);

  let alice_ui = refreshed_ui(&mut alice, channel).await;
  let appointed = row(&alice_ui, channel, bob_id);
  assert_eq!(appointed.role, ChannelRole::Admin);
  assert_eq!(
    appointed.maybe_appointer,
    Some(RoleAppointer {
      user: carol_id,
      authority: AppointerAuthority::WorkspaceAdmin,
    })
  );
  assert_eq!(alice_ui.user(carol_id).unwrap().display_name, "Carol");
  // Alice's own role is her founding claim at most: nobody appointed her.
  assert_eq!(row(&alice_ui, channel, alice_id).maybe_appointer, None);
}

/// Issue #1260, through the client: Eve demotes Bob and leaves the roster, and Carol reads the log
/// only then. She ends where everybody else is, only without the anchor.
#[tokio::test]
async fn a_late_reader_catches_up_after_a_signer_left_the_roster() {
  let server = FakeServer::new();
  let mut world = LateWorld::new(&server).await;
  let channel = world.anchored_channel().await;
  let (bob_id, carol_id) = (world.bob.client.user_id(), world.carol.client.user_id());

  let signing = world
    .eve
    .client
    .sign_channel_roles(
      channel,
      &[(bob_id, ChannelRole::Member), (carol_id, ChannelRole::Moderator)],
      false,
    )
    .await
    .unwrap();
  assert_eq!(signing, RoleSigning::Signed);
  world.alice.client.sync_channel_roles(channel).await.unwrap();
  world.eve_leaves().await;
  world.carol.client.sync_channel_roles(channel).await.unwrap();

  let state = world.carol.client.channel_role_state(channel).unwrap();
  assert_eq!(
    state.head(),
    world.alice.client.channel_role_state(channel).unwrap().head()
  );
  assert_eq!(state.role_of(bob_id), ChannelRole::Member, "the demotion counts");
  assert!(!state.is_workspace_anchored);
  assert!(!state.has_unverified_stretch());
  let carol_ui = refreshed_ui(&mut world.carol, channel).await;
  assert_eq!(row(&carol_ui, channel, carol_id).maybe_appointer, None);
}

/// A device that read the log on the roster without Eve can't place her entry, which comes in late.
/// It shows that, doesn't let the old snapshot's owner approve anything, and a workspace admin's
/// entry repairs it. Chat goes on.
#[tokio::test]
async fn a_stretch_a_device_cannot_verify_fails_closed_until_a_checkpoint() {
  let server = FakeServer::new();
  let mut world = LateWorld::new(&server).await;
  let channel = world.anchored_channel().await;
  let (alice_id, bob_id) = (world.alice.client.user_id(), world.bob.client.user_id());

  world.eve_leaves().await;
  world.bob.client.sync_workspace().await.unwrap();
  world.bob.client.sync_channel_roles(channel).await.unwrap();
  assert!(
    !world
      .bob
      .client
      .channel_role_state(channel)
      .unwrap()
      .has_unverified_stretch()
  );
  // Eve's device hasn't heard yet, and her entry reaches the server before the time the roster says.
  let signing = world
    .eve
    .client
    .sign_channel_roles(channel, &[(bob_id, ChannelRole::Member)], false)
    .await
    .unwrap();
  assert_eq!(signing, RoleSigning::Signed);
  world.bob.client.sync_channel_roles(channel).await.unwrap();

  let state = world.bob.client.channel_role_state(channel).unwrap();
  assert!(state.has_unverified_stretch());
  assert!(!state.is_workspace_anchored);
  assert!(state.is_signed());
  assert_eq!(state.admins(), Vec::new());
  assert_eq!(
    world.bob.client.verified_channel_role(channel, bob_id).unwrap(),
    ChannelRole::Member,
    "Bob counts for no approval"
  );
  let bob_ui = refreshed_ui(&mut world.bob, channel).await;
  assert!(
    bob_ui.channel_roles_review(channel).is_none(),
    "nobody asks him for anything"
  );

  // Chat isn't touched.
  world
    .alice
    .client
    .send_text(channel, None, "still here")
    .await
    .unwrap();
  settle(&mut [&mut world.alice, &mut world.bob]).await;
  world.bob.client.send_text(channel, None, "me too").await.unwrap();
  world.bob.client.flush_outbox().await.unwrap();

  // Alice, the workspace's owner, took Eve's entries on their dates, so her device won't vouch for
  // the snapshot unseen. She signs the list she looked at: the stretch is over.
  assert_eq!(
    world
      .alice
      .client
      .sign_channel_roles(channel, &[(bob_id, ChannelRole::Admin)], false)
      .await
      .unwrap(),
    RoleSigning::Unsigned
  );
  let reviewed = [(alice_id, ChannelRole::Admin), (bob_id, ChannelRole::Admin)];
  let at_cursor = world.alice.client.channel_role_state(channel).unwrap().cursor;
  assert_eq!(
    world
      .alice
      .client
      .sign_reviewed_channel_roles(channel, &reviewed, at_cursor)
      .await
      .unwrap(),
    RoleSigning::Signed
  );
  world.bob.client.sync_channel_roles(channel).await.unwrap();
  let state = world.bob.client.channel_role_state(channel).unwrap();
  assert!(!state.has_unverified_stretch());
  assert!(state.is_workspace_anchored);
  assert_eq!(state.role_of(alice_id), ChannelRole::Admin);
}

/// Variant A of laundering an anchor: a workspace admin's device took an entry on its dates, the
/// snapshot names somebody nobody vouched for, and her ordinary signature would anchor that on every
/// device. It signs nothing unasked (an import has nobody to ask); what she reviewed is signed exactly.
#[tokio::test]
async fn a_workspace_admin_never_vouches_for_a_snapshot_resting_on_somebody_who_left() {
  let server = FakeServer::new();
  let mut world = LateWorld::new(&server).await;
  let channel = world.anchored_channel().await;
  let (alice_id, bob_id, carol_id) = ids(&world.alice, &world.bob, &world.carol);
  world.carol.client.sync_channel_roles(channel).await.unwrap();

  let signing = world
    .eve
    .client
    .sign_channel_roles(channel, &[(carol_id, ChannelRole::Admin)], false)
    .await
    .unwrap();
  assert_eq!(signing, RoleSigning::Signed);
  world.eve_leaves().await;
  world.alice.client.sync_channel_roles(channel).await.unwrap();
  assert_eq!(
    world
      .alice
      .client
      .verified_channel_role(channel, carol_id)
      .unwrap(),
    ChannelRole::Admin
  );
  let before = server.role_log(channel).len();

  let alice_ui = world.ui_of_alice();
  assert!(alice_ui.channel_roles_review(channel).is_some());
  assert_eq!(
    world
      .alice
      .client
      .sign_channel_roles(channel, &[(bob_id, ChannelRole::Moderator)], false)
      .await
      .unwrap(),
    RoleSigning::Unsigned,
    "no entry says more than she saw"
  );
  assert_eq!(server.role_log(channel).len(), before);

  // She looked and left Carol out.
  let reviewed = [(alice_id, ChannelRole::Admin), (bob_id, ChannelRole::Admin)];
  let at_cursor = world.alice.client.channel_role_state(channel).unwrap().cursor;
  world
    .alice
    .client
    .sign_reviewed_channel_roles(channel, &reviewed, at_cursor)
    .await
    .unwrap();
  world.carol.client.sync_channel_roles(channel).await.unwrap();
  let state = world.carol.client.channel_role_state(channel).unwrap();
  assert_eq!(state.role_of(carol_id), ChannelRole::Member);
  assert!(state.is_workspace_anchored);
  assert!(
    !world
      .ui_of_alice()
      .channel_roles_review(channel)
      .unwrap()
      .needs_a_look
  );
}

/// Variant B, honest: the workspace admin's device is in a stretch and its snapshot names a demoted
/// owner. "Sign role" for somebody would have signed him again, anchored, on every device.
#[tokio::test]
async fn sign_role_during_a_stretch_does_not_re_sign_a_demoted_owner() {
  let server = FakeServer::new();
  let mut world = LateWorld::new(&server).await;
  let channel = world.anchored_channel().await;
  let (alice_id, bob_id, carol_id) = ids(&world.alice, &world.bob, &world.carol);
  world.alice.client.sync_channel_roles(channel).await.unwrap();

  world.eve_leaves().await;
  world.alice.client.sync_channel_roles(channel).await.unwrap();
  world
    .eve
    .client
    .sign_channel_roles(channel, &[(bob_id, ChannelRole::Member)], false)
    .await
    .unwrap();
  world.alice.client.sync_channel_roles(channel).await.unwrap();
  assert!(
    world
      .alice
      .client
      .channel_role_state(channel)
      .unwrap()
      .has_unverified_stretch()
  );
  let before = server.role_log(channel).len();

  assert_eq!(
    world
      .alice
      .client
      .set_channel_role(channel, carol_id, ChannelRole::Moderator)
      .await
      .unwrap(),
    RoleSigning::Unsigned
  );
  assert_eq!(server.role_log(channel).len(), before);
  let alice_ui = world.ui_of_alice();
  assert!(alice_ui.channel_roles_review(channel).is_some());

  let at_cursor = world.alice.client.channel_role_state(channel).unwrap().cursor;
  world
    .alice
    .client
    .sign_reviewed_channel_roles(
      channel,
      &[(alice_id, ChannelRole::Admin), (carol_id, ChannelRole::Moderator)],
      at_cursor,
    )
    .await
    .unwrap();
  let state = world.alice.client.channel_role_state(channel).unwrap();
  assert!(!state.has_unverified_stretch());
  assert_eq!(state.role_of(bob_id), ChannelRole::Member, "not signed back in");
}

/// Eve signs after the roster that removed her, and the server dates it so. Her entry is junk on every
/// device alike, whether or not it read the log since; the anchor stays and no stretch starts.
#[tokio::test]
async fn an_entry_after_the_roster_that_ended_a_tenure_is_junk_everywhere() {
  let server = FakeServer::new();
  let mut world = LateWorld::new(&server).await;
  let channel = world.anchored_channel().await;
  let carol_id = world.carol.client.user_id();
  world.eve.client.sync_channel_roles(channel).await.unwrap();
  world.eve_leaves().await;
  world.bob.client.sync_workspace().await.unwrap();
  world.bob.client.sync_channel_roles(channel).await.unwrap();
  server.advance_clock(std::time::Duration::from_secs(2 * 60 * 60));
  world.clock.advance(2 * 60 * 60 * 1000);

  world
    .eve
    .client
    .sign_channel_roles(channel, &[(carol_id, ChannelRole::Admin)], false)
    .await
    .unwrap();
  for peer in [&mut world.bob, &mut world.carol] {
    peer.client.sync_channel_roles(channel).await.unwrap();
    let state = peer.client.channel_role_state(channel).unwrap();
    assert_eq!(state.role_of(carol_id), ChannelRole::Member, "{}", peer.name);
    assert!(state.is_workspace_anchored, "{}", peer.name);
    assert!(!state.has_unverified_stretch(), "{}", peer.name);
  }
}

/// A failed read of the roster leaves the entries unread, not judged on an old roster and refused for
/// good: an entry by a newly promoted admin stays good, and no stretch comes of a network failure.
#[tokio::test]
async fn a_failed_roster_read_leaves_the_entries_unread() {
  let server = FakeServer::new();
  let mut world = LateWorld::new(&server).await;
  let channel = world.anchored_channel().await;
  let (alice_id, bob_id, carol_id) = ids(&world.alice, &world.bob, &world.carol);
  let eve_id = world.eve.client.user_id();
  let dave = LateWorld::enroll(&server, &world.clock, "Dave").await;
  let dave_id = dave.client.user_id();
  let mut dave = dave;
  world
    .alice
    .client
    .publish_roster(&[
      (alice_id, RosterRole::Owner),
      (eve_id, RosterRole::Admin),
      (dave_id, RosterRole::Admin),
    ])
    .await
    .unwrap();
  server.set_admin(dave_id);
  dave.client.full_sync().await.unwrap();
  dave.has_attested_keys_of(&server, &[carol_id]);
  let signing = dave
    .client
    .sign_channel_roles(channel, &[(carol_id, ChannelRole::Moderator)], false)
    .await
    .unwrap();
  assert_eq!(signing, RoleSigning::Signed);

  let before = world.carol.client.channel_role_state(channel).unwrap();
  server.fail_workspace_log(Some(WorkspaceLogFailure::Every));
  assert!(world.carol.client.sync_channel_roles(channel).await.is_err());
  assert_eq!(world.carol.client.channel_role_state(channel).unwrap(), before);

  server.fail_workspace_log(None);
  world.carol.client.sync_channel_roles(channel).await.unwrap();
  let state = world.carol.client.channel_role_state(channel).unwrap();
  assert_eq!(state.role_of(carol_id), ChannelRole::Moderator);
  assert_eq!(state.role_of(bob_id), ChannelRole::Admin);
  assert!(state.is_workspace_anchored);
  assert!(!state.has_unverified_stretch());
}

/// The same for the replay of a device from before the history.
#[tokio::test]
async fn a_failed_replay_of_the_roster_leaves_the_entries_unread() {
  let server = FakeServer::new();
  let mut world = LateWorld::new(&server).await;
  let channel = world.anchored_channel().await;
  let bob_id = world.bob.client.user_id();
  forget_admin_history(&world.carol);
  world
    .eve
    .client
    .sign_channel_roles(channel, &[(bob_id, ChannelRole::Member)], false)
    .await
    .unwrap();
  world.eve_leaves().await;
  world.carol.client.sync_workspace().await.unwrap();

  let before = world.carol.client.channel_role_state(channel).unwrap();
  server.fail_workspace_log(Some(WorkspaceLogFailure::RosterFromTheStart));
  assert!(world.carol.client.sync_channel_roles(channel).await.is_err());
  assert_eq!(world.carol.client.channel_role_state(channel).unwrap(), before);

  server.fail_workspace_log(None);
  world.carol.client.sync_channel_roles(channel).await.unwrap();
  assert_eq!(
    world.carol.client.verified_channel_role(channel, bob_id).unwrap(),
    ChannelRole::Member
  );
}

/// After an admin left the roster each channel's log is read, a few per pass, so an entry the server
/// shows later can't pass for one from before the removal; and then nothing is read any more.
#[tokio::test]
async fn a_removal_has_the_channels_logs_read_a_few_at_a_time() {
  let server = FakeServer::new();
  let mut world = LateWorld::new(&server).await;
  let channel = world.anchored_channel().await;
  world.eve_leaves().await;
  world.carol.client.sync_workspace().await.unwrap();
  let roster = stored_workspace(&world.carol)["roster"]["version"]
    .as_u64()
    .unwrap();
  assert_ne!(
    world
      .carol
      .client
      .channel_role_state(channel)
      .unwrap()
      .maybe_read_at_roster_version,
    Some(roster)
  );

  server.fail_role_log(channel, true);
  world.carol.client.read_due_channel_roles().await.unwrap();
  assert_eq!(due_role_reads(&world.carol).len(), 1, "stays due");
  server.fail_role_log(channel, false);
  // It waits for its retry, without asking the server.
  let reads = server.role_log_reads(channel);
  world.carol.client.read_due_channel_roles().await.unwrap();
  assert_eq!(server.role_log_reads(channel), reads);
  world.clock.advance(5 * 60 * 1000);
  world.carol.client.read_due_channel_roles().await.unwrap();
  assert_eq!(
    world
      .carol
      .client
      .channel_role_state(channel)
      .unwrap()
      .maybe_read_at_roster_version,
    Some(roster)
  );
  assert!(due_role_reads(&world.carol).is_empty());
  // And then nothing is read any more.
  let reads = server.role_log_reads(channel);
  world.carol.client.read_due_channel_roles().await.unwrap();
  assert_eq!(server.role_log_reads(channel), reads);
}

/// A replay that didn't end on the verified head isn't asked for again at the same roster: reading
/// the log from the start would fail here, and nothing asks.
#[tokio::test]
async fn a_replay_that_stayed_incomplete_waits_for_a_newer_roster() {
  let server = FakeServer::new();
  let mut world = LateWorld::new(&server).await;
  let channel = world.anchored_channel().await;
  let bob_id = world.bob.client.user_id();
  world.carol.client.sync_workspace().await.unwrap();
  let roster = stored_workspace(&world.carol)["roster"]["version"].clone();
  let mut state = stored_workspace(&world.carol);
  state["admin_history"] = serde_json::json!({ "is_complete": false, "tenures": [], "replayed_at": roster });
  world
    .carol
    .store
    .put(Table::Workspace, "state", &serde_json::to_vec(&state).unwrap())
    .unwrap();
  world
    .eve
    .client
    .sign_channel_roles(channel, &[(bob_id, ChannelRole::Moderator)], false)
    .await
    .unwrap();

  server.fail_workspace_log(Some(WorkspaceLogFailure::RosterFromTheStart));
  world.carol.client.sync_channel_roles(channel).await.unwrap();
  assert_eq!(
    world.carol.client.verified_channel_role(channel, bob_id).unwrap(),
    ChannelRole::Moderator
  );
}

/// One channel that keeps failing holds up no other, and waits longer and longer.
#[tokio::test]
async fn a_failing_channel_does_not_starve_the_read_queue() {
  let server = FakeServer::new();
  let mut world = LateWorld::new(&server).await;
  let mut channels = vec![world.anchored_channel().await];
  let space = world.alice.space().await;
  for name in ["a", "b", "c", "d", "e"] {
    channels.push(world.alice.client.create_channel(space, name).await.unwrap());
    let channel = *channels.last().unwrap();
    world
      .alice
      .client
      .add_member(channel, world.carol.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut world.alice, &mut world.carol]).await;
    world.carol.client.sync_channel_roles(channel).await.unwrap();
  }
  world.eve_leaves().await;
  world.carol.client.sync_workspace().await.unwrap();
  // Whichever comes first in the queue fails for good.
  let due = due_role_reads(&world.carol);
  let carols_channels: Vec<ConversationId> = due.keys().copied().collect();
  let failing = carols_channels[0];
  server.fail_role_log(failing, true);
  let base = server.role_log_reads(failing);

  world.carol.client.read_due_channel_roles().await.unwrap();
  world.carol.client.read_due_channel_roles().await.unwrap();
  assert_eq!(
    due_role_reads(&world.carol).keys().copied().collect::<Vec<_>>(),
    vec![failing],
    "the others got their turn"
  );
  assert_eq!(
    server.role_log_reads(failing),
    base + 1,
    "one failure, then it waits"
  );

  world.clock.advance(5 * 60 * 1000);
  world.carol.client.read_due_channel_roles().await.unwrap();
  assert_eq!(server.role_log_reads(failing), base + 2);
  world.clock.advance(5 * 60 * 1000);
  world.carol.client.read_due_channel_roles().await.unwrap();
  assert_eq!(server.role_log_reads(failing), base + 2, "the wait doubled");
  world.clock.advance(5 * 60 * 1000);
  world.carol.client.read_due_channel_roles().await.unwrap();
  assert_eq!(server.role_log_reads(failing), base + 3);
}

/// What was reviewed isn't signed on a log the reviewer didn't see: not when the log moved since they
/// looked, not when another admin gets there first. It goes back to them, and nothing is signed.
#[tokio::test]
async fn a_reviewed_list_is_not_signed_again_on_a_log_nobody_looked_at() {
  let server = FakeServer::new();
  let mut world = LateWorld::new(&server).await;
  let channel = world.anchored_channel().await;
  let (alice_id, bob_id, carol_id) = ids(&world.alice, &world.bob, &world.carol);
  world
    .eve
    .client
    .sign_channel_roles(channel, &[(carol_id, ChannelRole::Admin)], false)
    .await
    .unwrap();
  world.eve_leaves().await;
  world.alice.client.sync_channel_roles(channel).await.unwrap();
  let reviewed = [(alice_id, ChannelRole::Admin), (bob_id, ChannelRole::Admin)];
  let at_cursor = world.alice.client.channel_role_state(channel).unwrap().cursor;
  let ui = world.ui_of_alice();
  assert_eq!(ui.channel_roles_review(channel).unwrap().log_cursor, at_cursor);

  // Another admin got there first, between the read and the append.
  server.conflict_next_role_append();
  let before = server.role_log(channel).len();
  assert_eq!(
    world
      .alice
      .client
      .sign_reviewed_channel_roles(channel, &reviewed, at_cursor)
      .await
      .unwrap(),
    RoleSigning::ReviewOutdated
  );
  assert_eq!(server.role_log(channel).len(), before);

  // The log moved while she looked: an entry she didn't see.
  let founding = server.role_log(channel).remove(0);
  server.plant_role_log_entry(
    channel,
    founding.payload.as_slice().to_vec(),
    founding.signature.as_slice().to_vec(),
  );
  let before = server.role_log(channel).len();
  assert_eq!(
    world
      .alice
      .client
      .sign_reviewed_channel_roles(channel, &reviewed, at_cursor)
      .await
      .unwrap(),
    RoleSigning::ReviewOutdated
  );
  assert_eq!(server.role_log(channel).len(), before);

  // She looks again, and it goes through.
  let at_cursor = world
    .ui_of_alice()
    .channel_roles_review(channel)
    .unwrap()
    .log_cursor;
  assert_eq!(
    world
      .alice
      .client
      .sign_reviewed_channel_roles(channel, &reviewed, at_cursor)
      .await
      .unwrap(),
    RoleSigning::Signed
  );
}

/// A device from before the admin history replays the roster log once, when it first needs it.
#[tokio::test]
async fn a_device_from_before_the_history_gets_it_from_the_roster_log() {
  let server = FakeServer::new();
  let mut world = LateWorld::new(&server).await;
  let channel = world.anchored_channel().await;
  let bob_id = world.bob.client.user_id();
  forget_admin_history(&world.carol);
  let signing = world
    .eve
    .client
    .sign_channel_roles(channel, &[(bob_id, ChannelRole::Member)], false)
    .await
    .unwrap();
  assert_eq!(signing, RoleSigning::Signed);
  world.eve_leaves().await;
  world.carol.client.sync_channel_roles(channel).await.unwrap();

  let state = world.carol.client.channel_role_state(channel).unwrap();
  assert_eq!(state.role_of(bob_id), ChannelRole::Member);
  assert!(!state.has_unverified_stretch());
  assert!(
    stored_workspace(&world.carol)["admin_history"]["is_complete"]
      .as_bool()
      .unwrap()
  );
}

/// The channels whose role logs are due, as this device stored them.
fn due_role_reads(peer: &Peer) -> std::collections::BTreeMap<ConversationId, serde_json::Value> {
  let bytes = peer
    .store
    .get(Table::Workspace, "channel_roles_due")
    .unwrap()
    .unwrap();
  serde_json::from_slice(&bytes).unwrap()
}

/// The workspace state as this device stored it.
fn stored_workspace(peer: &Peer) -> serde_json::Value {
  let bytes = peer.store.get(Table::Workspace, "state").unwrap().unwrap();
  serde_json::from_slice(&bytes).unwrap()
}

/// Makes the stored workspace state look like one from before the admin history.
fn forget_admin_history(peer: &Peer) {
  let mut state = stored_workspace(peer);
  state.as_object_mut().unwrap().remove("admin_history");
  peer
    .store
    .put(Table::Workspace, "state", &serde_json::to_vec(&state).unwrap())
    .unwrap();
}

/// Alice owns the workspace, Eve is a workspace admin, Bob and Carol are members; everybody's clock
/// agrees with the server's, which dates the role entries.
struct LateWorld<'a> {
  server: &'a FakeServer,
  clock: Arc<ManualClock>,
  alice: Peer,
  eve: Peer,
  bob: Peer,
  carol: Peer,
}

impl<'a> LateWorld<'a> {
  async fn enroll(server: &FakeServer, clock: &Arc<ManualClock>, name: &str) -> Peer {
    Peer::enroll_on_clock(
      server.add_user(name),
      name,
      common::settings(),
      Box::new(clock.clone()),
    )
    .await
  }

  async fn new(server: &'a FakeServer) -> Self {
    let clock = Arc::new(ManualClock::new(server.server_now_ms()));
    let mut alice = Self::enroll(server, &clock, "Alice").await;
    let mut eve = Self::enroll(server, &clock, "Eve").await;
    let bob = Self::enroll(server, &clock, "Bob").await;
    let carol = Self::enroll(server, &clock, "Carol").await;
    let (alice_id, eve_id) = (alice.client.user_id(), eve.client.user_id());
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (eve_id, RosterRole::Admin)])
      .await
      .unwrap();
    server.set_admin(eve_id);
    eve.client.full_sync().await.unwrap();
    Self {
      server,
      clock,
      alice,
      eve,
      bob,
      carol,
    }
  }

  /// Alice's channel with Bob an admin Eve appointed; Bob and Carol read it, anchored.
  async fn anchored_channel(&mut self) -> ConversationId {
    self.bob.client.full_sync().await.unwrap();
    self.carol.client.full_sync().await.unwrap();
    let channel = channel_with_everybody(
      self.server,
      &mut self.alice,
      &mut [&mut self.bob, &mut self.carol],
    )
    .await;
    let bob_id = self.bob.client.user_id();
    // Eve isn't in the group: she signs for people she can check by their attested keys.
    self.eve.has_attested_keys_of(
      self.server,
      &[self.alice.client.user_id(), bob_id, self.carol.client.user_id()],
    );
    let signing = self
      .eve
      .client
      .sign_channel_roles(channel, &[(bob_id, ChannelRole::Admin)], false)
      .await
      .unwrap();
    assert_eq!(signing, RoleSigning::Signed);
    self
      .server
      .set_channel_role_unsigned(channel, bob_id, ChannelRole::Admin);
    for peer in [&mut self.bob, &mut self.carol] {
      peer.client.sync_channel_roles(channel).await.unwrap();
      let state = peer.client.channel_role_state(channel).unwrap();
      assert!(state.is_workspace_anchored);
      assert_eq!(state.role_of(bob_id), ChannelRole::Admin);
    }
    channel
  }

  /// Alice's UI on the same clock.
  fn ui_of_alice(
    &self,
  ) -> teamcraft_core::ui_backend::CoreChatBackend<Arc<teamcraft_core::store::MemoryStore>> {
    teamcraft_core::ui_backend::CoreChatBackend::new(
      self.alice.store.clone(),
      self.alice.client.user_id(),
      Box::new(self.clock.clone()),
    )
  }

  /// The next roster leaves Eve out, signed an hour on. The server's clock doesn't move with the
  /// test's: what Eve signs next still passes as dated inside her tenure, as with an owner's clock
  /// running ahead. `server.advance_clock` is the time really passing.
  async fn eve_leaves(&mut self) {
    let alice_id = self.alice.client.user_id();
    self.clock.advance(60 * 60 * 1000);
    self
      .alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
  }
}

async fn trio(server: &FakeServer) -> (Peer, Peer, Peer) {
  (
    Peer::enroll(server, "Alice").await,
    Peer::enroll(server, "Bob").await,
    Peer::enroll(server, "Carol").await,
  )
}

fn ids(alice: &Peer, bob: &Peer, carol: &Peer) -> (UserId, UserId, UserId) {
  (
    alice.client.user_id(),
    bob.client.user_id(),
    carol.client.user_id(),
  )
}

/// A public channel Alice creates and the others join.
async fn channel_with_everybody(
  server: &FakeServer,
  alice: &mut Peer,
  others: &mut [&mut Peer],
) -> ConversationId {
  let _ = server;
  let space = alice.space().await;
  let channel = create_public_channel(alice, space, "Ops").await;
  for peer in others.iter_mut() {
    let ui = backend_of(peer);
    ui.join_channel(channel);
    run_commands(peer, &ui).await;
  }
  let mut peers: Vec<&mut Peer> = vec![alice];
  peers.extend(others.iter_mut().map(|peer| &mut **peer));
  settle(&mut peers).await;
  for peer in &mut peers {
    peer.client.flush_outbox().await.unwrap();
  }
  settle(&mut peers).await;
  channel
}

async fn create_public_channel(peer: &mut Peer, space: SpaceId, name: &str) -> ConversationId {
  let ui = backend_of(peer);
  ui.create_channel_with(NewChannel {
    maybe_idempotency_key: None,
    space,
    name: name.into(),
    is_private: false,
    maybe_topic: None,
    maybe_category: None,
    maybe_history: None,
    is_integration: false,
  });
  run_commands(peer, &ui).await;
  ui.space_conversations(space)
    .into_iter()
    .find(|conversation| conversation.name == name)
    .unwrap()
    .id
}

/// The UI's view after the members section asked for the members and signed roles afresh.
async fn refreshed_ui(
  peer: &mut Peer,
  channel: ConversationId,
) -> teamcraft_core::ui_backend::CoreChatBackend<std::sync::Arc<teamcraft_core::store::MemoryStore>> {
  let ui = backend_of(peer);
  ui.refresh_channel_roles(channel);
  run_commands(peer, &ui).await;
  ui
}

fn row(ui: &impl ChatBackend, channel: ConversationId, user: UserId) -> ChannelMember {
  ui.channel_roles(channel)
    .into_iter()
    .find(|member| member.user == user)
    .unwrap()
}
