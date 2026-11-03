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
  // Never silent (admin.md §2.4): the member's device shows it once, from the commit it made
  // (issue #811, tests/suite/admin_joins.rs); the admin's own device joined by the welcome.
  let joined = teamcraft_core::store::ConversationEvent::AdminJoined {
    user: bob_id,
    is_approved: false,
  };
  for (peer, expected) in [(&carol, 1), (&bob, 0)] {
    let shown = peer
      .store
      .messages(channel, teamcraft_core::store::MessageFilter::TopLevel)
      .unwrap()
      .into_iter()
      .filter(|message| {
        matches!(&message.content, teamcraft_core::store::MessageContent::Event { event } if *event == joined)
      })
      .count();
    assert_eq!(shown, expected, "{}", peer.name);
  }
}

/// #775: under `visible` with the default policy (full history for roster members) an admin's
/// self-join signed full history, which every committer refuses (a self-add may only ask for the
/// history from the join on, e2ee.md §4.7): the admin stayed out for good.
#[tokio::test]
async fn an_admin_joins_by_themselves_under_the_default_full_history() {
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
    .publish_policy(WorkspacePolicy {
      admin_self_join: AdminSelfJoin::Visible,
      history_default: HistoryPolicy::Full,
      ..WorkspacePolicy::default()
    })
    .await
    .unwrap();
  server.set_admin(bob_id);
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  let channel = carol
    .client
    .create_channel(carol.space().await, "private")
    .await
    .unwrap();
  carol
    .client
    .send_text(channel, None, "before the admin came")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;

  bob.client.admin_add_member(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  assert!(
    bob.client.has_group(channel).unwrap(),
    "the self-join went through"
  );
  carol
    .client
    .send_text(channel, None, "after the admin came")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  bob.client.load_older_messages(channel).await.unwrap();
  assert_eq!(
    bob.texts(channel),
    vec!["after the admin came"],
    "a self-join reads from the join on"
  );
}

/// Issue #907: the members refused an admin's self-join (here the policy forbade it at the time;
/// in the field an older app signed full history). Nothing tells the admin's device, so it shows
/// that it waits and posts the self-add again later, which lets it in once the members accept it.
mod refused_self_join {
  use std::sync::Arc;

  use teamcraft_core::client::is_waiting_for_admission;
  use teamcraft_core::clock::ManualClock;
  use teamcraft_core::store::Table;

  use super::*;

  #[tokio::test]
  async fn the_self_add_is_posted_again_when_due_and_lets_the_admin_in() {
    let (clock, mut alice, mut bob, mut carol, channel) = refused().await;
    assert!(
      is_waiting_for_admission(&bob.store, channel).unwrap(),
      "shown as waiting"
    );
    assert_eq!(bob.client.retry_due_self_adds().await.unwrap(), 0, "not due yet");

    alice
      .client
      .publish_policy(policy(AdminSelfJoin::Visible))
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    clock.advance(retry_ms());
    assert_eq!(bob.client.retry_due_self_adds().await.unwrap(), 1);
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert!(
      bob.client.has_group(channel).unwrap(),
      "let in by the second self-add"
    );
    assert!(!is_waiting_for_admission(&bob.store, channel).unwrap());
    clock.advance(retry_ms() * 100);
    assert_eq!(
      bob.client.retry_due_self_adds().await.unwrap(),
      0,
      "nothing left to ask"
    );
  }

  #[tokio::test]
  async fn a_refused_self_add_waits_longer_after_each_attempt() {
    let (clock, mut alice, mut bob, mut carol, channel) = refused().await;
    clock.advance(retry_ms());
    assert_eq!(bob.client.retry_due_self_adds().await.unwrap(), 1);
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert!(!bob.client.has_group(channel).unwrap(), "refused again");

    clock.advance(retry_ms());
    assert_eq!(
      bob.client.retry_due_self_adds().await.unwrap(),
      0,
      "the second wait is twice as long"
    );
    clock.advance(retry_ms());
    assert_eq!(bob.client.retry_due_self_adds().await.unwrap(), 1);
    assert!(is_waiting_for_admission(&bob.store, channel).unwrap());
  }

  /// An earlier version marked the channel once and never asked again: after the update the next
  /// sync posts the self-add at once.
  #[tokio::test]
  async fn the_mark_of_an_earlier_version_is_posted_again_on_the_next_sync() {
    let (_clock, mut alice, mut bob, mut carol, channel) = refused().await;
    bob
      .store
      .delete(Table::SyncState, &format!("self_add/{channel}"))
      .unwrap();
    bob
      .store
      .put(Table::SyncState, &format!("self-add/{channel}"), b"true")
      .unwrap();
    alice
      .client
      .publish_policy(policy(AdminSelfJoin::Visible))
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    bob.client.full_sync().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert!(bob.client.has_group(channel).unwrap());
    assert!(!is_waiting_for_admission(&bob.store, channel).unwrap());
  }

  /// Alice owns the workspace, Bob is an admin whose self-join to Carol's private channel the
  /// members refused, on a clock the test moves.
  async fn refused() -> (
    Arc<ManualClock>,
    Peer,
    Peer,
    Peer,
    teamcraft_proto::ConversationId,
  ) {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll_on_clock(
      server.add_user("Bob"),
      "Bob",
      common::settings(),
      Box::new(clock.clone()),
    )
    .await;
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
    assert!(!bob.client.has_group(channel).unwrap(), "refused");
    (clock, alice, bob, carol, channel)
  }

  fn retry_ms() -> i64 {
    i64::try_from(common::settings().self_add_retry.as_millis()).unwrap()
  }
}

/// Issue #776 (admin.md §2.4): under `owner_approval` the admin asks, a channel owner's client
/// adds them with its own commit and tells everybody; the server's request is only a hint.
mod owner_approval {
  use teamcraft_core::read_state;
  use teamcraft_core::store::{ConversationEvent, MessageContent, MessageFilter};
  use teamcraft_core::{CoreError, JoinRequestRefusal};

  use super::*;

  #[tokio::test]
  async fn an_owner_approves_and_everybody_sees_the_admin_join_once() {
    let (server, mut alice, mut bob, mut carol, channel) = setup().await;
    let bob_id = bob.client.user_id();

    bob.client.request_admin_join(channel).await.unwrap();
    // Asking twice keeps one request.
    bob.client.request_admin_join(channel).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    let waiting = read_state::admin_join_requests(&carol.store).unwrap();
    assert_eq!(waiting.len(), 1, "the owner's device heard of it");
    assert_eq!(waiting[0].user.id, bob_id);
    assert!(
      read_state::admin_join_requests(&alice.store).unwrap().is_empty(),
      "not her channel"
    );
    #[cfg(feature = "ui")]
    {
      let shown = shown(&carol);
      assert_eq!(shown.len(), 1, "the owner's app offers it");
      assert_eq!(
        shown[0].standing,
        teamcraft_client_api::activity::JoinRequestStanding::Approvable
      );
    }

    carol.client.approve_admin_join(channel, bob_id).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert!(
      bob.client.has_group(channel).unwrap(),
      "the owner's commit let him in"
    );
    // The owner's device shows it from its own commit (issue #811); the admin joined by the
    // welcome. tests/suite/admin_joins.rs has the members' side.
    let joined = ConversationEvent::AdminJoined {
      user: bob_id,
      is_approved: true,
    };
    for (peer, expected) in [(&carol, 1), (&bob, 0)] {
      let shown = peer
        .store
        .messages(channel, MessageFilter::TopLevel)
        .unwrap()
        .into_iter()
        .filter(|message| matches!(&message.content, MessageContent::Event { event } if *event == joined))
        .count();
      assert_eq!(shown, expected, "{}", peer.name);
    }
    for peer in [&carol, &bob] {
      assert!(
        read_state::admin_join_requests(&peer.store).unwrap().is_empty(),
        "{}: settled",
        peer.name
      );
    }
    drop(server);
  }

  /// Review M2: an approval that went through already (a retry, another device) neither adds the
  /// admin again nor announces the join twice, and a decline afterwards changes nothing.
  #[tokio::test]
  async fn a_second_approval_neither_adds_nor_announces_again() {
    let (_server, mut alice, mut bob, mut carol, channel) = setup().await;
    let bob_id = bob.client.user_id();
    bob.client.request_admin_join(channel).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    carol.client.approve_admin_join(channel, bob_id).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    // The answer of the first one was lost: the owner approves again.
    carol.client.approve_admin_join(channel, bob_id).await.unwrap();
    carol.client.decline_admin_join(channel, bob_id).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert!(bob.client.has_group(channel).unwrap(), "the approval stands");
    assert_eq!(admin_joins(&carol, channel), 1, "one commit, one line");
    // The message for older versions went out once too.
    let announced = bob
      .store
      .messages(channel, MessageFilter::All)
      .unwrap()
      .into_iter()
      .filter(|message| {
        matches!(
          &message.content,
          MessageContent::Event {
            event: ConversationEvent::AdminJoined { .. }
          }
        )
      })
      .count();
    assert_eq!(announced, 1);
  }

  /// Review L4: approving shares what a self-join would get, the history from the join on, unless
  /// the owner explicitly shares the earlier history.
  #[tokio::test]
  async fn approving_shares_the_earlier_history_only_when_the_owner_chooses() {
    let (server, mut alice, mut bob, mut carol, channel) = setup_with(HistoryPolicy::Full).await;
    let bob_id = bob.client.user_id();
    carol.client.send_text(channel, None, "before").await.unwrap();
    bob.client.request_admin_join(channel).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    carol.client.approve_admin_join(channel, bob_id).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    bob.client.load_older_messages(channel).await.unwrap();
    assert!(bob.texts(channel).is_empty(), "nothing from before the join");

    let other = carol
      .client
      .create_channel(carol.space().await, "other")
      .await
      .unwrap();
    carol.client.send_text(other, None, "earlier").await.unwrap();
    bob.client.request_admin_join(other).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    carol
      .client
      .approve_admin_join_sharing(other, bob_id, HistoryPolicy::Full)
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    bob.client.load_older_messages(other).await.unwrap();
    assert_eq!(bob.texts(other), vec!["earlier"], "the owner chose to share it");
    drop(server);
  }

  /// Review M1: a request the server makes up for a DM is not shown, and approving it anyway adds
  /// nobody.
  #[tokio::test]
  async fn a_made_up_request_for_a_dm_is_neither_shown_nor_approved() {
    let (server, mut alice, mut bob, mut carol, _channel) = setup().await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    let dm = carol.client.open_dm(alice_id).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    server.plant_admin_join_request(dm, bob_id, &[alice_id]);
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert_eq!(
      read_state::admin_join_requests(&alice.store).unwrap().len(),
      1,
      "the server's list has it"
    );
    #[cfg(feature = "ui")]
    assert!(shown(&alice).is_empty(), "the app doesn't show it");
    let refused = alice.client.approve_admin_join(dm, bob_id).await;
    assert!(
      matches!(
        refused,
        Err(CoreError::JoinRequestNotApprovable(
          JoinRequestRefusal::NotAPrivateChannel
        ))
      ),
      "{refused:?}"
    );
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert!(!bob.client.has_group(dm).unwrap());
  }

  /// Review L1: the owner's app approves only while the policy it verified asks for approval.
  #[tokio::test]
  async fn approving_needs_the_verified_policy_to_ask_for_it() {
    let (_server, mut alice, mut bob, mut carol, channel) = setup().await;
    let bob_id = bob.client.user_id();
    bob.client.request_admin_join(channel).await.unwrap();
    alice
      .client
      .publish_policy(policy(AdminSelfJoin::Disabled))
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    let refused = carol.client.approve_admin_join(channel, bob_id).await;
    assert!(
      matches!(
        refused,
        Err(CoreError::JoinRequestNotApprovable(
          JoinRequestRefusal::PolicyWithoutApproval
        ))
      ),
      "{refused:?}"
    );
    #[cfg(feature = "ui")]
    assert!(shown(&carol).is_empty());
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert!(!bob.client.has_group(channel).unwrap());
  }

  #[tokio::test]
  async fn a_declined_request_is_gone_for_both_and_the_admin_stays_out() {
    let (_server, mut alice, mut bob, mut carol, channel) = setup().await;
    let bob_id = bob.client.user_id();

    bob.client.request_admin_join(channel).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    carol.client.decline_admin_join(channel, bob_id).await.unwrap();
    // Declined on another device already: nothing to do, no error.
    carol.client.decline_admin_join(channel, bob_id).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert!(read_state::admin_join_requests(&bob.store).unwrap().is_empty());
    assert!(read_state::admin_join_requests(&carol.store).unwrap().is_empty());
    assert!(!bob.client.has_group(channel).unwrap());
    // Approving what is gone fails clearly instead of adding him anyway.
    let late = carol.client.approve_admin_join(channel, bob_id).await;
    assert!(late.is_err(), "{late:?}");
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert!(!bob.client.has_group(channel).unwrap());
  }

  #[tokio::test]
  async fn somebody_only_the_server_calls_an_admin_is_not_let_in() {
    let (server, mut alice, mut bob, mut carol, channel) = setup().await;
    let mut dave = Peer::enroll(&server, "Dave").await;
    let dave_id = dave.client.user_id();
    // The server says Dave is an admin; the signed roster doesn't.
    server.set_admin(dave_id);
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    let refused = dave.client.request_admin_join(channel).await;
    assert!(
      matches!(refused, Err(teamcraft_core::CoreError::NotAnAdmin(user)) if user == dave_id),
      "{refused:?}"
    );
    let not_approved = carol.client.approve_admin_join(channel, dave_id).await;
    assert!(
      matches!(not_approved, Err(teamcraft_core::CoreError::NotAnAdmin(user)) if user == dave_id),
      "{not_approved:?}"
    );
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    assert!(!dave.client.has_group(channel).unwrap());
  }

  #[tokio::test]
  async fn the_admins_own_self_join_stays_refused_while_the_request_waits() {
    let (_server, mut alice, mut bob, mut carol, channel) = setup().await;
    let bob_id = bob.client.user_id();
    bob.client.request_admin_join(channel).await.unwrap();
    // A server that adds him anyway gains nothing: members refuse his own signed self-join.
    bob.client.admin_add_member(channel, bob_id).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert!(!bob.client.has_group(channel).unwrap());
  }

  /// The requests `peer`'s app shows to decide on.
  #[cfg(feature = "ui")]
  fn shown(peer: &Peer) -> Vec<teamcraft_client_api::activity::JoinRequest> {
    use teamcraft_client_api::backend::ChatBackend;
    common::backend_of(peer).join_requests()
  }

  /// How often the visible "joined as a workspace admin" line is in `peer`'s copy of `channel`.
  fn admin_joins(peer: &Peer, channel: teamcraft_proto::ConversationId) -> usize {
    peer
      .store
      .messages(channel, MessageFilter::TopLevel)
      .unwrap()
      .into_iter()
      .filter(|message| {
        matches!(
          &message.content,
          MessageContent::Event {
            event: ConversationEvent::AdminJoined { .. }
          }
        )
      })
      .count()
  }

  /// Alice owns the workspace, Bob is a signed admin, Carol founds (owns) a private channel; the
  /// policy asks for an owner's approval.
  async fn setup() -> (FakeServer, Peer, Peer, Peer, teamcraft_proto::ConversationId) {
    setup_with(HistoryPolicy::FromJoin).await
  }

  async fn setup_with(
    history_default: HistoryPolicy,
  ) -> (FakeServer, Peer, Peer, Peer, teamcraft_proto::ConversationId) {
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
      .publish_policy(WorkspacePolicy {
        history_default,
        ..policy(AdminSelfJoin::OwnerApproval)
      })
      .await
      .unwrap();
    server.set_admin(bob_id);
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    let channel = carol
      .client
      .create_channel(carol.space().await, "private")
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    (server, alice, bob, carol, channel)
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
