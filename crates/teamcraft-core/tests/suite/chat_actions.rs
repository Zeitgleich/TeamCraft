//! Message actions through the UI adapter and the fake server: replies, priority and
//! acknowledgements, edits with history, deletes, pins, forwards, "also send to channel",
//! permalinks and their previews, and marking unread.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::actions::{LinkedMessage, MessagePriority, OutgoingMessage};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{Bookmark, BookmarkKind, Message, MessageId, MessageKind, thread_id_of};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::ConversationId;

#[tokio::test]
async fn replies_priority_and_acknowledgements() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  bob_ui.post_message(channel, None, "who has the keys?");
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let question = last(&alice_ui, channel);

  alice_ui.send_message(OutgoingMessage {
    maybe_reply_to: Some(question.id),
    priority: MessagePriority::Urgent,
    wants_acknowledgement: true,
    ..outgoing(channel, "I do – please confirm you read this")
  });
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let answer = last(&bob_ui, channel);
  let extras = &bob_ui.message_extras(channel, &[answer.id])[0];
  let reply = extras.maybe_reply.as_ref().unwrap();
  assert_eq!(reply.excerpt, "who has the keys?");
  assert_eq!(reply.maybe_message, Some(question.id));
  assert_eq!(extras.priority, MessagePriority::Urgent);
  assert!(extras.wants_acknowledgement);

  bob_ui.acknowledge(channel, answer.id);
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let seen = &alice_ui.message_extras(channel, &[answer.id])[0];
  assert_eq!(seen.acknowledged_by, vec![bob.client.user_id()]);
}

#[tokio::test]
async fn edits_keep_history_and_deletes_reach_everyone() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  alice_ui.post_message(channel, None, "meeting at 3");
  run_commands(&mut alice, &alice_ui).await;
  let id = last(&alice_ui, channel).id;
  alice_ui.edit_message(channel, id, "meeting at 4");
  alice_ui.edit_message(channel, id, "meeting at 5");
  // Applied locally at once.
  assert_eq!(last(&alice_ui, channel).body, "meeting at 5");
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let seen = last(&bob_ui, channel);
  assert_eq!(seen.body, "meeting at 5");
  assert!(seen.is_edited);
  let history: Vec<String> = bob_ui.message_extras(channel, &[seen.id])[0]
    .edits
    .iter()
    .map(|edit| edit.body.clone())
    .collect();
  assert_eq!(history, ["meeting at 3", "meeting at 4"]);

  // Bob can't edit or delete Alice's message; Alice can delete it for everyone.
  bob_ui.delete_message(channel, seen.id);
  bob_ui.edit_message(channel, seen.id, "hijacked");
  assert!(bob_ui.drain_commands().is_empty());
  alice_ui.delete_message(channel, id);
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  assert!(bob_ui.message_extras(channel, &[seen.id])[0].is_deleted);
}

#[tokio::test]
async fn bookmarks_replace_the_bar_for_every_member() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  alice_ui.post_message(channel, None, "the spec");
  run_commands(&mut alice, &alice_ui).await;
  let spec = last(&alice_ui, channel);
  let bookmark = |title: &str, url: &str, maybe_message: Option<MessageId>| Bookmark {
    emoji: "📘".into(),
    title: title.into(),
    kind: if maybe_message.is_some() {
      BookmarkKind::File
    } else {
      BookmarkKind::Link
    },
    url: url.into(),
    maybe_message,
  };

  alice_ui.set_bookmarks(
    channel,
    vec![
      bookmark("Runbook", "https://example.com/runbook", None),
      bookmark("spec.pdf", "", Some(spec.id)),
    ],
  );
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let seen = bob_ui.conversation(channel).unwrap().bookmarks;
  assert_eq!(
    seen.iter().map(|b| b.title.as_str()).collect::<Vec<_>>(),
    ["Runbook", "spec.pdf"]
  );
  assert_eq!(seen[0].url, "https://example.com/runbook");
  assert_eq!(seen[1].maybe_message, Some(last(&bob_ui, channel).id));

  // The newest list wins, also for reordering and removal.
  bob_ui.set_bookmarks(channel, vec![seen[1].clone()]);
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let titles: Vec<String> = alice_ui
    .conversation(channel)
    .unwrap()
    .bookmarks
    .into_iter()
    .map(|b| b.title)
    .collect();
  assert_eq!(titles, ["spec.pdf"]);
}

#[tokio::test]
async fn pins_forwards_and_channel_copies_of_thread_replies() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  let other = alice
    .client
    .create_channel(alice.space().await, "elsewhere")
    .await
    .unwrap();
  alice
    .client
    .add_member(other, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));

  alice_ui.post_message(channel, None, "the plan");
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let plan = last(&bob_ui, channel);

  bob_ui.set_pinned(channel, plan.id, true);
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(alice_ui.pinned_messages(channel)[0].body, "the plan");

  bob_ui.forward_message(channel, plan.id, other, "fyi");
  bob_ui.send_message(OutgoingMessage {
    maybe_thread: Some(thread_id_of(plan.id)),
    also_send_to_channel: true,
    ..outgoing(channel, "agreed, also for everyone")
  });
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let forwarded = last(&alice_ui, other);
  assert_eq!(forwarded.body, "fyi");
  let origin = alice_ui.message_extras(other, &[forwarded.id])[0]
    .maybe_forwarded
    .clone()
    .unwrap();
  assert_eq!(origin.author_name, "Alice");
  assert_eq!(origin.maybe_conversation_name.as_deref(), Some("actions"));

  let top: Vec<String> = user_messages(&alice_ui, channel)
    .into_iter()
    .map(|message| message.body)
    .collect();
  assert_eq!(top, ["the plan", "agreed, also for everyone"]);
  assert_eq!(alice_ui.thread(channel, thread_id_of(plan.id)).len(), 2);
}

#[tokio::test]
async fn permalinks_resolve_for_members_and_unread_marks_sync() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let (alice_ui, bob_ui, carol_ui) = (backend_of(&alice), backend_of(&bob), backend_of(&carol));
  for text in ["one", "two", "three"] {
    alice_ui.post_message(channel, None, text);
  }
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;

  let two = user_messages(&bob_ui, channel)[1].clone();
  let link = bob_ui.permalink(channel, two.id).unwrap();
  let target = alice_ui.resolve_permalink(&link).unwrap();
  assert_eq!((target.conversation, target.message), (channel, two.id));
  assert_eq!(carol_ui.resolve_permalink(&link), None, "not a member");
  assert_eq!(bob_ui.resolve_permalink("teamcraft:/c/1/x/2"), None);

  bob_ui.mark_read(channel);
  run_commands(&mut bob, &bob_ui).await;
  assert_eq!(bob_ui.first_unread(channel), None);
  bob_ui.mark_unread(channel, two.id);
  run_commands(&mut bob, &bob_ui).await;
  assert_eq!(bob_ui.first_unread(channel), Some(two.id));
  assert_eq!(bob_ui.conversation(channel).unwrap().unread_count, 2);
  bob_ui.mark_read(channel);
  run_commands(&mut bob, &bob_ui).await;
  assert_eq!(bob_ui.first_unread(channel), None);
}

#[tokio::test]
async fn linked_messages_preview_for_members_and_follow_edits_and_deletes() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let (alice_ui, bob_ui, carol_ui) = (backend_of(&alice), backend_of(&bob), backend_of(&carol));
  alice_ui.post_message(channel, None, "the **plan**");
  run_commands(&mut alice, &alice_ui).await;
  let plan = last(&alice_ui, channel);
  alice_ui.post_message(channel, Some(thread_id_of(plan.id)), "a detail");
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  let link = alice_ui.permalink(channel, plan.id).unwrap();

  let Some(LinkedMessage::Visible { target, message, .. }) = bob_ui.linked_message(&link) else {
    panic!("a member sees the linked message");
  };
  assert_eq!((target.conversation, target.message), (channel, plan.id));
  assert_eq!(
    (message.body.as_str(), message.author),
    ("the **plan**", alice.client.user_id())
  );
  assert_eq!(
    carol_ui.linked_message(&link),
    Some(LinkedMessage::Unavailable),
    "not a member"
  );
  assert_eq!(
    bob_ui.linked_message("teamcraft:/c/1/m/2"),
    Some(LinkedMessage::Unavailable)
  );
  assert_eq!(
    bob_ui.linked_message("https://example.com/"),
    Some(LinkedMessage::Unavailable)
  );

  // A thread reply says where it lives.
  let detail = alice_ui.thread(channel, thread_id_of(plan.id))[1].clone();
  let reply_link = alice_ui.permalink(channel, detail.id).unwrap();
  let Some(LinkedMessage::Visible { message, .. }) = bob_ui.linked_message(&reply_link) else {
    panic!("a member sees the thread reply");
  };
  assert_eq!(message.maybe_thread, Some(thread_id_of(plan.id)));

  alice_ui.edit_message(channel, plan.id, "the new plan");
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let Some(LinkedMessage::Visible { message, .. }) = bob_ui.linked_message(&link) else {
    panic!("still visible after the edit");
  };
  assert_eq!(message.body, "the new plan");

  alice_ui.delete_message(channel, plan.id);
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  assert!(matches!(
    bob_ui.linked_message(&link),
    Some(LinkedMessage::Deleted { .. })
  ));
}

async fn channel_with_two() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "actions")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  (server, alice, bob, channel)
}

fn outgoing(conversation: ConversationId, body: &str) -> OutgoingMessage {
  OutgoingMessage {
    conversation,
    maybe_thread: None,
    body: body.into(),
    maybe_reply_to: None,
    priority: MessagePriority::Normal,
    wants_acknowledgement: false,
    is_silent: false,
    also_send_to_channel: false,
    files: Vec::new(),
  }
}

/// The conversation's messages without system messages (such as "Bob joined").
fn user_messages(backend: &impl ChatBackend, conversation: ConversationId) -> Vec<Message> {
  backend
    .messages(conversation, 0, 100)
    .into_iter()
    .filter(|message| message.kind == MessageKind::User)
    .collect()
}

fn last(backend: &impl ChatBackend, conversation: ConversationId) -> Message {
  let count = backend.message_count(conversation);
  backend.messages(conversation, count - 1, 1).remove(0)
}
