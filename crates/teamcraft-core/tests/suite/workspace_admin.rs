//! Workspace management through the UI adapter and the fake server: channels with options, the
//! channel browser and self-join, channel updates announced as system messages, categories,
//! invitations, adding members with a history choice, and the external-people badge.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::actions::{ChannelChanges, NewChannel, NewSpace, SpaceInvite, WorkspaceEvent};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{Message, MessageKind, SpaceRoster, SystemEvent};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::{ChannelRole, ConversationId, HistoryPolicy, IdempotencyKey, SpaceId, SpaceRole};

#[tokio::test]
async fn public_channels_are_browsed_and_joined() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let space = alice.space().await;
  let channel = create_public_channel(&mut alice, space, "Ops").await;

  let bob_ui = backend_of(&bob);
  assert!(
    bob_ui.browse_channels(space).is_empty(),
    "the first look only asks the core to load the browser"
  );
  run_commands(&mut bob, &bob_ui).await;
  let listed = bob_ui.browse_channels(space);
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].conversation, channel);
  assert_eq!(listed[0].topic, "Daily operations");
  assert!(!listed[0].is_member);

  bob_ui.join_channel(channel);
  run_commands(&mut bob, &bob_ui).await;
  settle_and_flush(&mut [&mut alice, &mut bob]).await;

  // Alice committed Bob's signed self-add; his device has the keys and announced the join.
  let alice_ui = backend_of(&alice);
  let joined = system_messages(&alice_ui, channel);
  assert!(
    joined
      .iter()
      .any(|message| message.kind == MessageKind::System(SystemEvent::Joined)
        && message.author == bob.client.user_id())
  );
  bob_ui.post_message(channel, None, "glad to be here");
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  assert!(alice.texts(channel).contains(&"glad to be here".to_string()));
  run_commands(&mut bob, &bob_ui).await;
  assert!(
    bob_ui
      .browse_channels(space)
      .iter()
      .all(|listed| listed.is_member)
  );
}

#[tokio::test]
async fn channel_changes_are_announced() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let space = alice.space().await;
  let channel = create_public_channel(&mut alice, space, "Ops").await;
  let alice_ui = backend_of(&alice);
  alice_ui.add_member_with_history(channel, bob.client.user_id(), HistoryPolicy::Full);
  run_commands(&mut alice, &alice_ui).await;
  settle_and_flush(&mut [&mut alice, &mut bob]).await;

  alice_ui.update_channel(
    channel,
    ChannelChanges {
      maybe_name: Some("Operations".into()),
      maybe_topic: Some("Weekly operations".into()),
      maybe_is_private: Some(true),
      ..ChannelChanges::default()
    },
  );
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let bob_ui = backend_of(&bob);
  let kinds: Vec<MessageKind> = system_messages(&bob_ui, channel)
    .into_iter()
    .map(|message| message.kind)
    .collect();
  assert!(kinds.contains(&MessageKind::System(SystemEvent::Renamed {
    name: "Operations".into()
  })));
  assert!(kinds.contains(&MessageKind::System(SystemEvent::TopicChanged {
    topic: "Weekly operations".into()
  })));
  let events = workspace_events(&bob_ui, channel);
  assert!(events.contains(&WorkspaceEvent::MadePrivate {
    by: alice.client.user_id()
  }));
  assert!(events.contains(&WorkspaceEvent::MemberAdded {
    user: bob.client.user_id(),
    by: alice.client.user_id(),
    history: HistoryPolicy::Full,
  }));
}

/// The server made the channel but the answer was lost: the core retries with the same key and
/// ends up with the one channel.
#[tokio::test]
async fn a_lost_create_answer_is_retried_without_a_duplicate() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let space = alice.space().await;

  server.lose_create_answers(1);
  let channel = alice.client.create_channel(space, "lossy").await.unwrap();

  assert_eq!(server.channel_count("lossy"), 1);
  assert!(alice.client.has_group(channel).unwrap());
}

/// A queued create action that is retried (a lost answer, a second click) carries the key it was
/// made with, so the server creates the space or channel once.
#[tokio::test]
async fn a_retried_create_makes_one_space_and_one_channel() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let ui = backend_of(&alice);
  let space_key = Some(IdempotencyKey::from_bytes([1; 16]));
  let channel_key = Some(IdempotencyKey::from_bytes([2; 16]));
  let space = alice.space().await;

  for _ in 0..2 {
    ui.create_space(NewSpace {
      maybe_idempotency_key: space_key,
      name: "Project C".into(),
      maybe_description: None,
      is_open: true,
    });
    ui.create_channel_with(NewChannel {
      maybe_idempotency_key: channel_key,
      space,
      name: "retried".into(),
      is_private: false,
      maybe_topic: None,
      maybe_category: None,
      maybe_history: None,
      is_integration: false,
    });
    run_commands(&mut alice, &ui).await;
  }

  let spaces = ui.spaces().into_iter().filter(|s| s.name == "Project C").count();
  let channels = ui
    .space_conversations(space)
    .into_iter()
    .filter(|c| c.name == "retried")
    .count();
  assert_eq!((spaces, channels), (1, 1));
}

#[tokio::test]
async fn a_moved_channel_keeps_its_group_and_is_announced() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let source = alice.space().await;
  let channel = create_public_channel(&mut alice, source, "Ops").await;
  let alice_ui = backend_of(&alice);
  alice_ui.add_member_with_history(channel, bob.client.user_id(), HistoryPolicy::Full);
  run_commands(&mut alice, &alice_ui).await;
  settle_and_flush(&mut [&mut alice, &mut bob]).await;

  alice_ui.create_space(NewSpace {
    maybe_idempotency_key: None,
    name: "Project B".into(),
    maybe_description: None,
    is_open: true,
  });
  run_commands(&mut alice, &alice_ui).await;
  let target = alice_ui
    .spaces()
    .into_iter()
    .find(|space| space.name == "Project B")
    .unwrap()
    .id;

  alice_ui.update_channel(
    channel,
    ChannelChanges {
      maybe_space: Some(target),
      ..ChannelChanges::default()
    },
  );
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  assert!(alice_ui.space_conversations(source).is_empty());
  assert_eq!(alice_ui.space_conversations(target)[0].id, channel);
  // The encryption group did not change: messages still flow after the move.
  alice_ui.post_message(channel, None, "after the move");
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  assert!(bob.texts(channel).contains(&"after the move".to_string()));
  let bob_ui = backend_of(&bob);
  assert!(
    workspace_events(&bob_ui, channel).contains(&WorkspaceEvent::Moved {
      by: alice.client.user_id(),
      space: "Project B".into(),
    })
  );
}

#[tokio::test]
async fn members_added_from_join_miss_earlier_messages() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let space = alice.space().await;
  let channel = create_public_channel(&mut alice, space, "Ops").await;
  let alice_ui = backend_of(&alice);
  alice_ui.post_message(channel, None, "before Carol");
  run_commands(&mut alice, &alice_ui).await;

  alice_ui.add_member_with_history(channel, carol.client.user_id(), HistoryPolicy::FromJoin);
  run_commands(&mut alice, &alice_ui).await;
  settle_and_flush(&mut [&mut alice, &mut carol]).await;
  alice_ui.post_message(channel, None, "after Carol");
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut carol]).await;

  assert_eq!(carol.texts(channel), ["after Carol"]);
  assert!(
    workspace_events(&alice_ui, channel).contains(&WorkspaceEvent::MemberAdded {
      user: carol.client.user_id(),
      by: alice.client.user_id(),
      history: HistoryPolicy::FromJoin,
    })
  );
}

#[tokio::test]
async fn adding_to_a_channel_brings_people_into_the_space_first() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let space = alice.space().await;
  let channel = create_public_channel(&mut alice, space, "Ops").await;
  server.make_space_outsider(carol.client.user_id());
  let alice_ui = backend_of(&alice);

  alice_ui.add_people_to_channel(
    channel,
    vec![(carol.client.user_id(), false)],
    HistoryPolicy::FromJoin,
    true,
  );
  run_commands(&mut alice, &alice_ui).await;
  settle_and_flush(&mut [&mut alice, &mut carol]).await;
  alice_ui.post_message(channel, None, "welcome");
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut carol]).await;

  let adds = server.space_member_adds();
  assert_eq!(adds.len(), 1, "{adds:?}");
  assert_eq!(adds[0].1.user_id, carol.client.user_id());
  assert_eq!(carol.texts(channel), ["welcome"]);
}

#[tokio::test]
async fn categories_invitations_and_external_badge() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let space = alice.space().await;
  let channel = create_public_channel(&mut alice, space, "Ops").await;
  let alice_ui = backend_of(&alice);

  assert!(alice_ui.categories(space).is_empty());
  alice_ui.create_category(space, "Projects");
  run_commands(&mut alice, &alice_ui).await;
  let categories = alice_ui.categories(space);
  assert_eq!(categories.len(), 1);
  alice_ui.rename_category(space, categories[0].id, "Project A");
  run_commands(&mut alice, &alice_ui).await;
  assert_eq!(alice_ui.categories(space)[0].name, "Project A");
  alice_ui.delete_category(space, categories[0].id);
  run_commands(&mut alice, &alice_ui).await;
  assert!(alice_ui.categories(space).is_empty());

  alice_ui.invite_to_space(SpaceInvite {
    space,
    email: "guest@example.com".into(),
    is_guest: true,
    channels: vec![channel],
    maybe_expires_at: None,
    maybe_message: None,
  });
  run_commands(&mut alice, &alice_ui).await;
  let invites = server.invites();
  assert_eq!(invites.len(), 1);
  assert_eq!(invites[0].role, SpaceRole::Guest);
  assert_eq!(invites[0].channel_ids, [channel]);

  assert!(!alice_ui.has_external_people(channel));
  server.set_contains_guests(channel);
  alice.client.full_sync().await.unwrap();
  assert!(alice_ui.has_external_people(channel));
}

#[tokio::test]
async fn people_from_the_directory_join_a_space_once() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob = server.add_user("Bob");
  let carol = server.add_user("Carol");
  let space = alice.space().await;
  // Bob is not in the space yet; Carol already is and must keep her role.
  server.make_space_outsider(bob.user_id());

  let alice_ui = backend_of(&alice);
  alice_ui.add_space_members(space, vec![bob.user_id(), carol.user_id(), bob.user_id()], false);
  run_commands(&mut alice, &alice_ui).await;

  let adds = server.space_member_adds();
  assert_eq!(adds.len(), 1, "{adds:?}");
  assert_eq!(adds[0].0, space);
  assert_eq!(adds[0].1.user_id, bob.user_id());
  assert_eq!(adds[0].1.role, SpaceRole::Member);
}

#[tokio::test]
async fn guests_are_added_with_the_guest_role_and_invitations_carry_the_message() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let guest = server.add_user("Guest");
  let space = alice.space().await;
  server.make_space_outsider(guest.user_id());

  let alice_ui = backend_of(&alice);
  alice_ui.add_space_members(space, vec![guest.user_id()], true);
  alice_ui.invite_to_space(SpaceInvite {
    space,
    email: "new@example.com".into(),
    is_guest: false,
    channels: Vec::new(),
    maybe_expires_at: None,
    maybe_message: Some("Welcome to Project A".into()),
  });
  run_commands(&mut alice, &alice_ui).await;

  assert_eq!(server.space_member_adds()[0].1.role, SpaceRole::Guest);
  let invites = server.invites();
  assert_eq!(invites.len(), 1);
  assert_eq!(invites[0].role, SpaceRole::Member);
  assert_eq!(
    server.invite_messages(),
    [Some("Welcome to Project A".to_owned())]
  );
}

#[tokio::test]
async fn space_settings_list_change_and_remove_members_and_withdraw_invitations() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob = server.add_user("Bob");
  let space = alice.space().await;
  let alice_ui = backend_of(&alice);
  alice_ui.invite_to_space(SpaceInvite {
    space,
    email: "new@example.com".into(),
    is_guest: false,
    channels: Vec::new(),
    maybe_expires_at: None,
    maybe_message: None,
  });
  run_commands(&mut alice, &alice_ui).await;

  assert_eq!(
    alice_ui.space_members(space),
    SpaceRoster::Loading,
    "nothing until the settings ask"
  );
  alice_ui.refresh_space_roster(space);
  run_commands(&mut alice, &alice_ui).await;
  let members = loaded(alice_ui.space_members(space));
  assert!(
    members
      .iter()
      .any(|member| member.user == bob.user_id() && member.role == SpaceRole::Member)
  );
  let invites = alice_ui.space_invites(space);
  assert_eq!(invites.len(), 1);
  assert_eq!(invites[0].email, "new@example.com");

  alice_ui.set_space_role(space, bob.user_id(), SpaceRole::SpaceAdmin);
  run_commands(&mut alice, &alice_ui).await;
  let bob_now = |ui: &dyn ChatBackend| {
    loaded(ui.space_members(space))
      .into_iter()
      .find(|member| member.user == bob.user_id())
  };
  assert_eq!(
    bob_now(&alice_ui).map(|member| member.role),
    Some(SpaceRole::SpaceAdmin)
  );

  alice_ui.remove_space_member(space, bob.user_id());
  alice_ui.revoke_space_invite(space, invites[0].id);
  run_commands(&mut alice, &alice_ui).await;
  assert_eq!(
    bob_now(&alice_ui),
    None,
    "the roster is reloaded after the removal"
  );
  assert!(alice_ui.space_invites(space).is_empty());
}

#[tokio::test]
async fn channel_admins_change_roles_and_the_member_list_shows_what_the_server_stored() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let space = alice.space().await;
  let channel = create_public_channel(&mut alice, space, "Ops").await;
  let bob_ui = backend_of(&bob);
  bob_ui.join_channel(channel);
  run_commands(&mut bob, &bob_ui).await;
  settle_and_flush(&mut [&mut alice, &mut bob]).await;
  let alice_ui = backend_of(&alice);
  let role_of = |ui: &dyn ChatBackend, user| {
    ui.channel_roles(channel)
      .into_iter()
      .find(|member| member.user == user)
      .map(|member| member.role)
  };
  assert_eq!(
    role_of(&alice_ui, alice.client.user_id()),
    Some(ChannelRole::Admin)
  );
  assert_eq!(
    role_of(&alice_ui, bob.client.user_id()),
    Some(ChannelRole::Member)
  );

  alice_ui.set_channel_role(channel, bob.client.user_id(), ChannelRole::Moderator);
  run_commands(&mut alice, &alice_ui).await;
  assert_eq!(
    role_of(&alice_ui, bob.client.user_id()),
    Some(ChannelRole::Moderator)
  );
}

#[tokio::test]
async fn role_changes_without_the_permission_or_for_oneself_are_refused() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let space = alice.space().await;
  let channel = create_public_channel(&mut alice, space, "Ops").await;
  let bob_ui = backend_of(&bob);
  bob_ui.join_channel(channel);
  run_commands(&mut bob, &bob_ui).await;
  settle_and_flush(&mut [&mut alice, &mut bob]).await;
  let alice_ui = backend_of(&alice);

  // Nobody changes their own role, not even a channel admin.
  let own_admin = alice
    .client
    .set_channel_role(channel, alice.client.user_id(), ChannelRole::Moderator)
    .await;
  assert!(own_admin.is_err());
  // A member can't promote themselves.
  let own = bob
    .client
    .set_channel_role(channel, bob.client.user_id(), ChannelRole::Admin)
    .await;
  assert!(own.is_err());
  // A moderator can't change roles either.
  alice
    .client
    .set_channel_role(channel, bob.client.user_id(), ChannelRole::Moderator)
    .await
    .unwrap();
  let demote = bob
    .client
    .set_channel_role(channel, alice.client.user_id(), ChannelRole::Member)
    .await;
  assert!(demote.is_err());
  let roles = alice_ui.channel_roles(channel);
  let held = |user| {
    roles
      .iter()
      .find(|member| member.user == user)
      .map(|member| member.role.clone())
  };
  assert_eq!(held(alice.client.user_id()), Some(ChannelRole::Admin));
  assert_eq!(held(bob.client.user_id()), Some(ChannelRole::Moderator));
}

#[tokio::test]
async fn the_people_of_my_spaces_are_known_without_a_shared_conversation() {
  let server = FakeServer::new();
  let alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let _ = alice.space().await;

  bob.client.full_sync().await.unwrap();

  // A new member can find and message colleagues before sharing any conversation with them.
  let bob_ui = backend_of(&bob);
  let names: Vec<String> = bob_ui.users().into_iter().map(|user| user.display_name).collect();
  assert!(names.contains(&"Alice".to_string()), "{names:?}");
}

fn loaded(roster: SpaceRoster) -> Vec<teamcraft_client_api::model::SpaceMember> {
  match roster {
    SpaceRoster::Loaded(members) => members,
    other => panic!("the roster is {other:?}"),
  }
}

async fn create_public_channel(peer: &mut Peer, space: SpaceId, name: &str) -> ConversationId {
  let ui = backend_of(peer);
  ui.create_channel_with(NewChannel {
    maybe_idempotency_key: None,
    space,
    name: name.into(),
    is_private: false,
    maybe_topic: Some("Daily operations".into()),
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

/// Settles, then sends what waited for keys (announcements queued before a device joined).
async fn settle_and_flush(peers: &mut [&mut Peer]) {
  for _ in 0..3 {
    settle(peers).await;
    for peer in peers.iter_mut() {
      peer.client.flush_outbox().await.unwrap();
    }
  }
  settle(peers).await;
}

fn system_messages(backend: &impl ChatBackend, conversation: ConversationId) -> Vec<Message> {
  all_messages(backend, conversation)
    .into_iter()
    .filter(|message| matches!(message.kind, MessageKind::System(_)))
    .collect()
}

fn workspace_events(backend: &impl ChatBackend, conversation: ConversationId) -> Vec<WorkspaceEvent> {
  let ids: Vec<_> = all_messages(backend, conversation)
    .iter()
    .map(|message| message.id)
    .collect();
  backend
    .message_extras(conversation, &ids)
    .into_iter()
    .filter_map(|extras| extras.maybe_workspace_event)
    .collect()
}

fn all_messages(backend: &impl ChatBackend, conversation: ConversationId) -> Vec<Message> {
  backend.messages(conversation, 0, backend.message_count(conversation))
}

#[tokio::test]
async fn someone_in_no_conversation_still_has_their_name() {
  let server = FakeServer::new();
  let mut erin = Peer::enroll(&server, "Erin").await;
  erin.client.full_sync().await.unwrap();

  let me = backend_of(&erin).me();
  assert_eq!(me.display_name, "Erin");
}
