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
use teamcraft_client_api::model::{Message, MessageKind, SystemEvent};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::{ConversationId, HistoryPolicy, SpaceId, SpaceRole};

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

async fn create_public_channel(peer: &mut Peer, space: SpaceId, name: &str) -> ConversationId {
  let ui = backend_of(peer);
  ui.create_channel_with(NewChannel {
    space,
    name: name.into(),
    is_private: false,
    maybe_topic: Some("Daily operations".into()),
    maybe_category: None,
    maybe_history: None,
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
