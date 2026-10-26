//! Signed workspace roster and policy, admin actions on groups the admin isn't in, and removals the
//! server asks for, through the fake server.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::LocalStore;
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::{AdminSelfJoin, HistoryPolicy, PolicyDocument, WorkspaceLogKind, WorkspacePolicy};

#[tokio::test]
async fn roster_and_policy_are_verified_and_forgeries_ignored() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());

  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Admin)])
    .await
    .unwrap();
  alice
    .client
    .publish_policy(policy(AdminSelfJoin::Disabled))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;

  let workspace = carol.client.workspace().unwrap();
  assert_eq!(
    workspace.maybe_root.as_ref().map(|root| root.user_id),
    Some(alice_id)
  );
  assert!(workspace.is_admin(bob_id));
  assert_eq!(workspace.policy.admin_self_join, AdminSelfJoin::Disabled);

  // The server appends a policy nobody signed: clients keep the verified one.
  let forged = PolicyDocument {
    version: 2,
    policy: policy(AdminSelfJoin::Visible),
  };
  server.inject_workspace_entry(
    WorkspaceLogKind::Policy,
    serde_json::to_vec(&forged).unwrap(),
    vec![1, 2, 3],
  );
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  let workspace = carol.client.workspace().unwrap();
  assert_eq!(workspace.policy.admin_self_join, AdminSelfJoin::Disabled);
  assert!(workspace.maybe_rejection.is_some());
}

mod last_owner {
  use super::*;

  #[tokio::test]
  async fn the_last_owner_cannot_be_removed_but_can_hand_over() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Admin)])
      .await
      .unwrap();

    // Demoting the only owner: refused before anything is signed (the fake server has no check).
    let refused = alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Admin), (bob_id, RosterRole::Admin)])
      .await;
    assert!(matches!(refused, Err(teamcraft_core::CoreError::LastOwner)));
    let refused = alice.client.publish_roster(&[]).await;
    assert!(matches!(refused, Err(teamcraft_core::CoreError::LastOwner)));
    assert_eq!(alice.client.workspace().unwrap().roster.version, 1);

    // The successor first, then the old owner goes (docs/admin.md, owner lifecycle).
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Owner)])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    bob
      .client
      .publish_roster(&[(bob_id, RosterRole::Owner)])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let workspace = alice.client.workspace().unwrap();
    assert_eq!(workspace.roster.version, 3);
    assert_eq!(workspace.maybe_rejection, None);
    assert!(!workspace.is_admin(alice_id));
    let status = workspace.owner_status(i64::MAX / 2).unwrap();
    assert_eq!(status.owners.len(), 1);
    assert_eq!(status.owners[0].0, bob_id);
    assert!(status.owners[0].1.is_some(), "bob's signing time is known");
  }

  #[tokio::test]
  async fn owners_promote_demote_and_remove_and_admins_cannot_sign() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let (alice_id, bob_id, carol_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      carol.client.user_id(),
    );
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();

    // Add an owner and an admin after the genesis.
    alice
      .client
      .publish_roster(&[
        (alice_id, RosterRole::Owner),
        (bob_id, RosterRole::Owner),
        (carol_id, RosterRole::Admin),
      ])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    let workspace = carol.client.workspace().unwrap();
    assert_eq!(workspace.maybe_rejection, None);
    assert_eq!(workspace.owner_status(i64::MAX / 2).unwrap().owners.len(), 2);
    assert!(workspace.is_admin(carol_id));

    // The admin can't sign a roster, not even one that makes them an owner.
    let refused = carol
      .client
      .publish_roster(&[
        (alice_id, RosterRole::Owner),
        (bob_id, RosterRole::Owner),
        (carol_id, RosterRole::Owner),
      ])
      .await;
    assert!(matches!(refused, Err(teamcraft_core::CoreError::NotAnOwner)));

    // Bob demotes Alice to admin and removes Carol.
    bob
      .client
      .publish_roster(&[(bob_id, RosterRole::Owner), (alice_id, RosterRole::Admin)])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    let workspace = carol.client.workspace().unwrap();
    assert_eq!(workspace.roster.version, 3);
    assert_eq!(workspace.maybe_rejection, None);
    let status = workspace.owner_status(i64::MAX / 2).unwrap();
    assert_eq!(
      status.owners.iter().map(|owner| owner.0).collect::<Vec<_>>(),
      vec![bob_id]
    );
    assert_eq!(status.admins, vec![alice_id]);
    assert!(!workspace.is_admin(carol_id));

    // Alice is no owner any more; Bob, the last one, can't remove himself.
    let refused = alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await;
    assert!(matches!(refused, Err(teamcraft_core::CoreError::NotAnOwner)));
    let refused = bob.client.publish_roster(&[(alice_id, RosterRole::Admin)]).await;
    assert!(matches!(refused, Err(teamcraft_core::CoreError::LastOwner)));
    assert_eq!(bob.client.workspace().unwrap().roster.version, 3);
  }

  #[tokio::test]
  async fn an_edit_of_an_older_roster_is_not_signed() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let (alice_id, bob_id, carol_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      carol.client.user_id(),
    );
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Owner)])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    // Both owners open the editor on version 1; Alice signs first.
    let base = bob.client.workspace().unwrap().roster.version;
    alice
      .client
      .publish_roster_based_on(
        base,
        &[
          (alice_id, RosterRole::Owner),
          (bob_id, RosterRole::Owner),
          (carol_id, RosterRole::Admin),
        ],
      )
      .await
      .unwrap();
    let stale = bob
      .client
      .publish_roster_based_on(
        base,
        &[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Owner)],
      )
      .await;
    assert!(matches!(
      stale,
      Err(teamcraft_core::CoreError::RosterBaseMismatch)
    ));
    let workspace = bob.client.workspace().unwrap();
    assert_eq!(workspace.roster.version, 2);
    assert!(workspace.is_admin(carol_id), "Alice's change was not undone");
    // From the fresh version it works.
    bob
      .client
      .publish_roster_based_on(2, &[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Owner)])
      .await
      .unwrap();
  }

  #[tokio::test]
  async fn a_zero_owner_roster_from_a_hostile_server_is_refused() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let alice_id = alice.client.user_id();
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut carol]).await;

    // The server appends a roster that continues the chain but names no owner.
    let mut orphaned = carol.client.workspace().unwrap().roster;
    orphaned.version = 2;
    orphaned.maybe_prev_hash = carol.client.workspace().unwrap().heads.maybe_roster_hash;
    for member in &mut orphaned.members {
      member.role = RosterRole::Admin;
    }
    server.inject_workspace_entry(
      WorkspaceLogKind::Roster,
      serde_json::to_vec(&orphaned).unwrap(),
      vec![1, 2, 3],
    );
    settle(&mut [&mut alice, &mut carol]).await;
    for peer in [&alice, &carol] {
      let workspace = peer.client.workspace().unwrap();
      assert_eq!(workspace.roster.version, 1, "the verified roster stays");
      assert!(workspace.is_admin(alice_id));
      assert!(
        workspace
          .maybe_rejection
          .as_deref()
          .is_some_and(|reason| reason.contains("has no owner")),
        "{:?}",
        workspace.maybe_rejection
      );
    }
  }
}

/// Issue #765: signed entries never disappear by themselves. The automatic re-sign signs exactly
/// the verified roster, and an older, validly signed roster the server serves again can't replace
/// a newer one.
mod roster_integrity {
  use super::*;
  use teamcraft_core::api::ServerApi;
  use teamcraft_proto::ListWorkspaceLogQuery;

  #[tokio::test]
  async fn the_automatic_re_sign_keeps_every_entry() {
    let server = FakeServer::new();
    let clock = std::sync::Arc::new(teamcraft_core::clock::ManualClock::new(1_800_000_000_000));
    let mut alice = Peer::enroll_on_clock(
      server.add_user("Alice"),
      "Alice",
      common::settings(),
      Box::new(clock.clone()),
    )
    .await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let mut dave = Peer::enroll(&server, "Dave").await;
    let roles = [
      (alice.client.user_id(), RosterRole::Owner),
      (bob.client.user_id(), RosterRole::Owner),
      (carol.client.user_id(), RosterRole::Admin),
      (dave.client.user_id(), RosterRole::Admin),
    ];
    alice.client.publish_roster(&roles).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    let before = alice.client.workspace().unwrap().roster;

    // Not due yet: nothing is signed.
    alice.client.refresh_workspace_signatures().await.unwrap();
    assert_eq!(alice.client.workspace().unwrap().roster.version, before.version);

    // Half the accepted age later the owner's client signs again, the same people in the same roles.
    let half = i64::try_from(common::settings().workspace_max_age.as_millis()).unwrap() / 2;
    clock.advance(half + 1);
    alice.client.refresh_workspace_signatures().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    for peer in [&alice, &bob, &carol, &dave] {
      let roster = peer.client.workspace().unwrap().roster;
      assert_eq!(roster.version, before.version + 1, "{}", peer.name);
      assert_eq!(roster.members, before.members, "{}: no entry dropped", peer.name);
    }
  }

  #[tokio::test]
  async fn an_older_signed_roster_served_again_is_refused() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Admin)])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    // The server appends the genesis (validly signed, without Bob) once more.
    let log = alice
      .api
      .list_workspace_log(&WorkspaceLogKind::Roster, &ListWorkspaceLogQuery::default())
      .await
      .unwrap();
    let genesis = log.entries.first().unwrap().clone();
    server.inject_workspace_entry(
      WorkspaceLogKind::Roster,
      genesis.payload.as_slice().to_vec(),
      genesis.signature.as_slice().to_vec(),
    );
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    for peer in [&alice, &carol] {
      let workspace = peer.client.workspace().unwrap();
      assert_eq!(workspace.roster.version, 2, "{}", peer.name);
      assert!(workspace.is_admin(bob_id), "{}: Bob stays", peer.name);
      assert!(workspace.maybe_rejection.is_some(), "{}", peer.name);
    }
  }
}

#[tokio::test]
async fn a_policy_edited_from_something_unverified_is_not_signed() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let alice_id = alice.client.user_id();
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner)])
    .await
    .unwrap();
  alice
    .client
    .publish_policy(policy(AdminSelfJoin::Disabled))
    .await
    .unwrap();
  let verified = alice.client.workspace().unwrap().policy;

  // The console showed a policy the server altered (self-join visible); the owner edits something else.
  let mut shown = verified.clone();
  shown.admin_self_join = AdminSelfJoin::Visible;
  let mut edited = shown.clone();
  edited.history_default = HistoryPolicy::FromJoin;
  let refused = alice.client.publish_policy_based_on(&shown, edited).await;
  assert!(matches!(
    refused,
    Err(teamcraft_core::CoreError::PolicyBaseMismatch)
  ));
  assert_eq!(
    alice.client.workspace().unwrap().policy_version,
    1,
    "nothing was signed"
  );

  // Edited from what the core verified: signed.
  let mut edited = verified.clone();
  edited.history_default = HistoryPolicy::FromJoin;
  alice
    .client
    .publish_policy_based_on(&verified, edited)
    .await
    .unwrap();
  assert_eq!(alice.client.workspace().unwrap().policy_version, 2);
}

mod genesis {
  use super::*;

  #[tokio::test]
  async fn the_first_policy_signs_only_the_confirmed_roster() {
    // #265: the server reports Mallory as an identity provider owner; the owner confirmed only Bob
    // as admin, so the genesis roster holds Alice and Bob and nobody else.
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let mut mallory = Peer::enroll(&server, "Mallory").await;
    let (alice_id, bob_id, mallory_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      mallory.client.user_id(),
    );
    let base = alice.client.workspace_policy().unwrap();
    alice
      .client
      .publish_policy_with_genesis(&base, policy(AdminSelfJoin::Disabled), &[], &[bob_id])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut mallory]).await;

    let workspace = mallory.client.workspace().unwrap();
    assert_eq!(workspace.roster.version, 1);
    let roles: Vec<_> = workspace
      .roster
      .members
      .iter()
      .map(|member| (member.user_id, member.role.clone()))
      .collect();
    assert_eq!(
      roles,
      vec![(alice_id, RosterRole::Owner), (bob_id, RosterRole::Admin)]
    );
    assert!(!workspace.is_admin(mallory_id));
    assert_eq!(workspace.policy.admin_self_join, AdminSelfJoin::Disabled);
  }

  #[tokio::test]
  async fn a_genesis_list_is_ignored_once_a_roster_is_verified() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mallory = Peer::enroll(&server, "Mallory").await;
    let alice_id = alice.client.user_id();
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    let base = alice.client.workspace_policy().unwrap();

    // A later save carrying an owner list (a forged one, say) changes the policy, not the roster.
    alice
      .client
      .publish_policy_with_genesis(
        &base,
        policy(AdminSelfJoin::Disabled),
        &[mallory.client.user_id()],
        &[],
      )
      .await
      .unwrap();
    let workspace = alice.client.workspace().unwrap();
    assert_eq!(workspace.roster.version, 1, "no roster was signed");
    assert_eq!(workspace.roster.members.len(), 1);
    assert_eq!(workspace.policy_version, 1);
  }
}

#[tokio::test]
async fn admins_cannot_sign_policy() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Admin)])
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  // Bob's own client refuses before signing.
  let refused = bob.client.publish_policy(policy(AdminSelfJoin::Visible)).await;
  assert!(matches!(refused, Err(teamcraft_core::CoreError::NotAnOwner)));

  // Nothing was signed; a non-owner's signature would be refused on verify anyway (unit tests).
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(alice.client.workspace().unwrap().policy_version, 0);
}

#[tokio::test]
async fn admin_adds_people_to_a_channel_they_are_not_in() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let mut dave = Peer::enroll(&server, "Dave").await;
  let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Admin)])
    .await
    .unwrap();
  server.set_admin(bob_id);
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  let channel = carol
    .client
    .create_channel(carol.space().await, "carols-channel")
    .await
    .unwrap();
  bob
    .client
    .admin_add_member(channel, dave.client.user_id())
    .await
    .unwrap();
  // Carol's client sees Bob's signed change, checks him against the roster and commits it.
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  assert!(dave.client.has_group(channel).unwrap());

  carol
    .client
    .send_text(channel, None, "welcome Dave")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  assert_eq!(dave.texts(channel), vec!["welcome Dave"]);
  assert!(!bob.client.has_group(channel).unwrap());
}

#[tokio::test]
async fn admin_self_join_follows_the_signed_policy() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Admin)])
    .await
    .unwrap();
  alice
    .client
    .publish_policy(policy(AdminSelfJoin::Disabled))
    .await
    .unwrap();
  server.set_admin(bob_id);
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  let channel = carol
    .client
    .create_channel(carol.space().await, "private")
    .await
    .unwrap();

  bob.client.admin_add_member(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  assert!(!bob.client.has_group(channel).unwrap(), "self-join is disabled");

  alice
    .client
    .publish_policy(policy(AdminSelfJoin::Visible))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  bob.client.admin_add_member(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  assert!(bob.client.has_group(channel).unwrap(), "self-join is visible");
  // Never silent (admin.md §2.4): the committing member tells everybody, once.
  let joined = teamcraft_core::store::ConversationEvent::AdminJoined { user: bob_id };
  for peer in [&carol, &bob] {
    let shown = peer
      .store
      .messages(channel, teamcraft_core::store::MessageFilter::TopLevel)
      .unwrap()
      .into_iter()
      .filter(|message| {
        matches!(&message.content, teamcraft_core::store::MessageContent::Event { event } if *event == joined)
      })
      .count();
    assert_eq!(shown, 1, "{}", peer.name);
  }
}

#[tokio::test]
async fn a_lost_device_is_revoked_by_its_owner_and_keys_rotate() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "devices")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  alice.client.upload_key_bag().await.unwrap();
  let mut laptop = Peer::enroll_as(alice.api.clone(), "Alice laptop").await;
  alice.client.add_missing_devices(channel).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut laptop]).await;
  assert!(laptop.client.has_group(channel).unwrap());
  let listed_with_laptop = server.device_list_of(alice.client.user_id()).unwrap();

  // The laptop is lost: Alice's phone revokes it, which signs its removal from the group and a
  // device list without it (security review A4, A8).
  alice
    .client
    .revoke_device(laptop.client.device_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut laptop]).await;
  bob
    .client
    .send_text(channel, None, "after the laptop was lost")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut laptop]).await;

  assert_eq!(alice.texts(channel), vec!["after the laptop was lost"]);
  assert!(laptop.texts(channel).is_empty());
  assert!(!laptop.client.has_group(channel).unwrap());
  let newest = server.device_list_of(alice.client.user_id()).unwrap();
  assert!(newest.sequence > listed_with_laptop.sequence);

  // The server goes back to the list that still had the laptop: Bob's client refuses it, and
  // adding devices goes on with the newest list it saw.
  server.serve_device_list(alice.client.user_id(), listed_with_laptop);
  assert_eq!(bob.client.add_missing_devices(channel).await.unwrap(), 0);
}

#[tokio::test]
async fn a_removal_the_server_reports_is_not_signed_by_members() {
  // Security review A8: the server can't make an honest client sign "Alice removed Carol".
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let channel = three_member_channel(&mut alice, &mut bob, &mut carol).await;
  let commits_before = commits(&alice, channel).await;

  server.remove_member_as_admin(channel, carol.client.user_id());
  settle(&mut [&mut alice, &mut bob]).await;
  alice.client.apply_pending_changes().await.unwrap();
  bob.client.apply_pending_changes().await.unwrap();

  assert_eq!(commits(&alice, channel).await, commits_before);
}

#[tokio::test]
async fn an_admin_removes_someone_with_a_signed_change() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let mut admin = Peer::enroll(&server, "Admin").await;
  let (alice_id, admin_id) = (alice.client.user_id(), admin.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (admin_id, RosterRole::Admin)])
    .await
    .unwrap();
  server.set_admin(admin_id);
  let channel = three_member_channel(&mut alice, &mut bob, &mut carol).await;
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut admin]).await;

  // The admin isn't in the channel; a member's client commits the admin's signed removal.
  admin
    .client
    .admin_remove_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  bob.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  alice
    .client
    .send_text(channel, None, "after Carol left")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;

  assert_eq!(bob.texts(channel), vec!["after Carol left"]);
  assert!(!carol.client.has_group(channel).unwrap());
}

async fn three_member_channel(
  alice: &mut Peer,
  bob: &mut Peer,
  carol: &mut Peer,
) -> teamcraft_proto::ConversationId {
  let channel = alice
    .client
    .create_channel(alice.space().await, "team")
    .await
    .unwrap();
  for user in [bob.client.user_id(), carol.client.user_id()] {
    alice.client.add_member(channel, user).await.unwrap();
  }
  settle(&mut [alice, bob, carol]).await;
  channel
}

async fn commits(peer: &Peer, conversation: teamcraft_proto::ConversationId) -> usize {
  use teamcraft_core::api::ServerApi;
  peer
    .api
    .list_mls_messages(conversation, &teamcraft_proto::ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .messages
    .iter()
    .filter(|message| message.kind == teamcraft_proto::MlsMessageKind::Commit)
    .count()
}

#[tokio::test]
async fn implicit_full_history_only_for_users_the_roster_vouches_for() {
  // Security review A9: the server's channel history policy and "not a guest" are unsigned.
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Admin)])
    .await
    .unwrap();
  let channel = alice
    .client
    .create_channel(alice.space().await, "history")
    .await
    .unwrap();
  alice
    .client
    .send_text(channel, None, "before anyone joined")
    .await
    .unwrap();

  for peer in [&bob, &carol] {
    alice
      .client
      .add_member(channel, peer.client.user_id())
      .await
      .unwrap();
  }
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  for peer in [&mut bob, &mut carol] {
    peer.client.load_older_messages(channel).await.unwrap();
  }

  assert_eq!(bob.texts(channel), vec!["before anyone joined"]);
  assert!(carol.texts(channel).is_empty(), "Carol isn't vouched for");
}

fn policy(admin_self_join: AdminSelfJoin) -> WorkspacePolicy {
  WorkspacePolicy {
    admin_self_join,
    history_default: HistoryPolicy::FromJoin,
    ..WorkspacePolicy::default()
  }
}

mod attestation {
  use std::future::Future;
  use std::pin::Pin;
  use std::sync::Arc;

  use base64::Engine as _;
  use base64::engine::general_purpose::URL_SAFE_NO_PAD;
  use p256::ecdsa::SigningKey;
  use p256::ecdsa::signature::Signer as _;
  use teamcraft_core::store::MemoryStore;
  use teamcraft_core::{AttestationSettings, ClientSettings, JwksSource, PendingEnrollment};

  use super::*;

  const ISSUER: &str = "https://id.example.com/realms/company";

  #[tokio::test]
  async fn only_attested_identity_keys_are_pinned_when_required() {
    // Security review A1: with attestation required, the server can't hand out an identity key
    // the identity provider didn't attest.
    let server = FakeServer::new();
    let provider = Arc::new(SigningKey::random(&mut rand_core::OsRng));
    let mut alice = enroll_attested(&server, "Alice", &provider, true).await;
    let bob = enroll_attested(&server, "Bob", &provider, true).await;
    let carol = enroll_attested(&server, "Carol", &provider, false).await;
    let channel = alice
      .create_channel(
        teamcraft_core::api::ServerApi::list_spaces(&server.add_user("Admin"))
          .await
          .unwrap()
          .spaces[0]
          .id,
        "attested",
      )
      .await
      .unwrap();

    alice.add_member(channel, bob.user_id()).await.unwrap();
    // Carol's key has no attestation: it isn't pinned, so she can't be added.
    assert!(matches!(
      alice.add_member(channel, carol.user_id()).await,
      Err(teamcraft_core::CoreError::Mls(
        teamcraft_crypto::mls::MlsError::UnknownUser(_)
      ))
    ));
  }

  async fn enroll_attested(
    server: &FakeServer,
    name: &str,
    provider: &Arc<SigningKey>,
    with_token: bool,
  ) -> common::TestClient {
    let api = server.add_user(name);
    let pending = PendingEnrollment::new();
    let mut options = common::options(common::PASSPHRASE);
    if with_token {
      options.maybe_attestation = Some(token(provider, &pending.attestation_claim(api.user_id())));
    }
    let settings = ClientSettings {
      maybe_attestation: Some(AttestationSettings {
        issuer: ISSUER.into(),
        audience: "teamcraft".into(),
        is_required: true,
      }),
      ..common::settings()
    };
    let mut client = pending
      .complete(
        api,
        Arc::new(MemoryStore::new()),
        common::clock(),
        settings,
        options,
      )
      .await
      .unwrap();
    client.set_jwks_source(Box::new(Jwks(provider.clone())));
    client
  }

  fn token(provider: &SigningKey, claim: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","kid":"k1"}"#);
    let claims =
      serde_json::json!({ "iss": ISSUER, "aud": "teamcraft", "sub": claim, "tc_identity_key": claim });
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let input = format!("{header}.{payload}");
    let signature: p256::ecdsa::Signature = provider.sign(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
  }

  struct Jwks(Arc<SigningKey>);

  impl JwksSource for Jwks {
    fn fetch(&self) -> Pin<Box<dyn Future<Output = Option<Vec<u8>>> + '_>> {
      let point = self.0.verifying_key().to_encoded_point(false);
      let jwks = serde_json::json!({ "keys": [{
        "kty": "EC", "crv": "P-256", "kid": "k1",
        "x": URL_SAFE_NO_PAD.encode(point.x().unwrap()),
        "y": URL_SAFE_NO_PAD.encode(point.y().unwrap()),
      }]});
      Box::pin(async move { Some(serde_json::to_vec(&jwks).unwrap()) })
    }
  }
}
