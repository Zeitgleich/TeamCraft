//! Signed workspace roster and policy, admin actions on groups the admin isn't in, and removals the
//! server asks for, through the fake server.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Peer, settle};
use teamcraft_core::api::fake::FakeServer;
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

  // The fake server accepts it (the real one checks the role, but must not be trusted to).
  bob
    .client
    .publish_policy(policy(AdminSelfJoin::Visible))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  let workspace = alice.client.workspace().unwrap();
  assert_eq!(workspace.policy_version, 0);
  assert!(
    workspace
      .maybe_rejection
      .unwrap()
      .contains("not validly signed by an owner")
  );
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
}

#[tokio::test]
async fn removed_device_is_committed_out_and_keys_rotate() {
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

  // The laptop is lost: the server revokes it, a member's client commits it out of the group.
  server.revoke_device(alice.client.user_id(), laptop.client.device_id());
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
}

#[tokio::test]
async fn server_side_removal_is_committed_and_keys_rotate() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "team")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;

  // An admin deactivates Carol on the server; members' clients finish it cryptographically.
  server.remove_member_as_admin(channel, carol.client.user_id());
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

fn policy(admin_self_join: AdminSelfJoin) -> WorkspacePolicy {
  WorkspacePolicy {
    admin_self_join,
    history_default: HistoryPolicy::FromJoin,
    ..WorkspacePolicy::default()
  }
}
