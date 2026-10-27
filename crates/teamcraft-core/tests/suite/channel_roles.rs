//! Signed channel roles (issue #380, e2ee.md §4.6.1) between clients and the fake server: the
//! creator founds the log, role changes are signed and verified by the other members, roles that
//! are the server's word only are shown as such and count for nothing, and a channel from before
//! signed roles is founded by its first admin who changes a role.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::actions::NewChannel;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::ChannelMember;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::RoleSigning;
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
  assert!(row(&bob_ui, channel, bob_id).is_role_signed);
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
