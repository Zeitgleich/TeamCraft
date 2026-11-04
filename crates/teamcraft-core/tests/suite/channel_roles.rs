//! Signed channel roles (issue #380, e2ee.md §4.6.1) between clients and the fake server: the
//! creator founds the log, role changes are signed and verified by the other members, roles that
//! are the server's word only are shown as such and count for nothing, and a channel from before
//! signed roles is founded by its first admin who changes a role.
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

  // The server makes Carol an admin on its own: she shows as one, marked, and counts as a member.
  server.set_channel_role_unsigned(channel, carol_id, ChannelRole::Admin);
  let bob_ui = refreshed_ui(&mut bob, channel).await;
  let carol_row = row(&bob_ui, channel, carol_id);
  assert_eq!(carol_row.role, ChannelRole::Admin);
  assert!(!carol_row.is_role_signed);
  assert_eq!(
    carol_row.maybe_appointer, None,
    "nobody verified names an appointer"
  );
  assert!(row(&bob_ui, channel, bob_id).is_role_signed);
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

  // A signed admin confirms the server's role; then it counts.
  let confirmed = alice
    .client
    .confirm_channel_role(channel, carol_id)
    .await
    .unwrap();
  assert_eq!(confirmed, RoleSigning::Signed);
  let bob_ui = refreshed_ui(&mut bob, channel).await;
  assert!(row(&bob_ui, channel, carol_id).is_role_signed);
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

#[tokio::test]
async fn a_channel_from_before_signed_roles_is_founded_by_its_first_admin_who_changes_a_role() {
  let server = FakeServer::new();
  let (mut alice, mut bob, mut carol) = trio(&server).await;
  // An older server: the channel is made without a role log.
  server.set_role_log_lacking(true);
  let channel = channel_with_everybody(&server, &mut alice, &mut [&mut bob, &mut carol]).await;
  let (alice_id, bob_id, carol_id) = ids(&alice, &bob, &carol);
  // Role changes keep working there, unsigned.
  let unsigned = alice
    .client
    .set_channel_role(channel, carol_id, ChannelRole::Moderator)
    .await
    .unwrap();
  assert_eq!(unsigned, RoleSigning::Unsigned);
  server.set_role_log_lacking(false);

  // The server's roles are shown, marked, and grant nothing.
  let bob_ui = refreshed_ui(&mut bob, channel).await;
  assert!(!row(&bob_ui, channel, alice_id).is_role_signed);
  assert!(!row(&bob_ui, channel, carol_id).is_role_signed);
  assert!(!bob.client.channel_role_state(channel).unwrap().is_signed());

  // Alice, admin by the server's list, is offered to sign; Bob isn't.
  assert!(backend_of(&alice).can_sign_channel_roles(channel));
  assert!(!bob_ui.can_sign_channel_roles(channel));

  // A member can't claim it: the server refuses, and nothing is signed.
  assert!(
    bob
      .client
      .set_channel_role(channel, bob_id, ChannelRole::Admin)
      .await
      .is_err()
  );

  // Alice, admin by the server's list, founds it with her next change. Carol's moderator role
  // stays the server's word until an admin confirms it.
  let founded = alice
    .client
    .set_channel_role(channel, bob_id, ChannelRole::Moderator)
    .await
    .unwrap();
  assert_eq!(founded, RoleSigning::Signed);
  let bob_ui = refreshed_ui(&mut bob, channel).await;
  assert!(row(&bob_ui, channel, alice_id).is_role_signed);
  assert!(row(&bob_ui, channel, bob_id).is_role_signed);
  assert!(!row(&bob_ui, channel, carol_id).is_role_signed);
  assert_eq!(
    bob.client.verified_channel_role(channel, alice_id).unwrap(),
    ChannelRole::Admin
  );
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
  assert!(
    !carol_ui.are_channel_roles_unverified(channel),
    "no word in an ordinary channel"
  );
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
  assert!(bob_ui.are_channel_roles_unverified(channel));
  assert!(!row(&bob_ui, channel, bob_id).is_role_signed);
  assert!(
    !bob_ui.can_sign_channel_roles(channel),
    "nor can he sign on a snapshot he can't vouch for"
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
  assert!(!alice_ui.can_sign_channel_roles(channel));
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
  assert!(world.ui_of_alice().channel_roles_review(channel).is_none());
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

  server.set_channel_role_unsigned(channel, carol_id, ChannelRole::Moderator);
  assert_eq!(
    world
      .alice
      .client
      .confirm_channel_role(channel, carol_id)
      .await
      .unwrap(),
    RoleSigning::Unsigned
  );
  assert_eq!(server.role_log(channel).len(), before);
  let alice_ui = world.ui_of_alice();
  assert!(alice_ui.channel_roles_review(channel).is_some());
  assert!(!alice_ui.can_sign_channel_roles(channel));

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
