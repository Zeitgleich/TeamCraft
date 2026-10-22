//! The activity feed, last seen and call history as the UI reads them: built from decrypted local
//! data and the server's answers, through the same adapter the desktop shell uses.
#![cfg(feature = "ui")]
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::activity::ActivityKind;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::calls::CallDirection;
use teamcraft_client_api::model::thread_id_of;
use teamcraft_client_api::privacy::LastSeenVisibility;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::{CallHistoryEntry, GatewayEvent, GatewayServerMessage, PresenceState, UnixMillis};

#[tokio::test]
async fn mentions_reactions_and_thread_replies_form_one_feed_with_an_unread_state() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
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
  settle(&mut [&mut alice, &mut bob]).await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));

  alice_ui.post_message(channel, None, "ship the release");
  alice_ui.post_message(channel, None, "what do you think?");
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  // Bob reacts to the first message, mentions Alice and replies in the thread of the second.
  let seen = bob_ui.messages(channel, 0, 10);
  let id_of = |body: &str| seen.iter().find(|message| message.body == body).unwrap().id;
  let (release, question) = (id_of("ship the release"), id_of("what do you think?"));
  bob_ui.toggle_reaction(channel, release, "🎉");
  bob_ui.post_message(channel, None, "@alice review please");
  bob_ui.post_message(channel, Some(thread_id_of(question)), "I think yes");
  // Bob's own entries never show up for Bob.
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  assert!(bob_ui.activity().items.is_empty());

  let feed = alice_ui.activity();
  assert_eq!(feed.items.len(), 3, "{feed:?}");
  assert_eq!(feed.unread_count, 3);
  assert!(feed.items.iter().all(|item| item.actor == bob.client.user_id()));
  let kind_of = |matching: fn(&ActivityKind) -> bool| feed.items.iter().find(|item| matching(&item.kind));
  let reaction = kind_of(|kind| matches!(kind, ActivityKind::Reaction { emoji } if emoji == "🎉")).unwrap();
  assert_eq!(reaction.message, release);
  assert_eq!(reaction.excerpt, "ship the release");
  let reply = kind_of(|kind| *kind == ActivityKind::ThreadReply).unwrap();
  assert_eq!(reply.maybe_thread, Some(thread_id_of(question)));
  assert_eq!(reply.excerpt, "I think yes");
  let mention = kind_of(|kind| *kind == ActivityKind::Mention).unwrap();
  assert_eq!(mention.excerpt, "@alice review please");

  // Reading is stored in the account data and moves the unread count to zero.
  alice_ui.mark_activity_read();
  run_commands(&mut alice, &alice_ui).await;
  let read = alice_ui.activity();
  assert_eq!(read.items.len(), 3);
  assert_eq!(read.unread_count, 0);
}

#[tokio::test]
async fn last_seen_comes_from_the_server_and_follows_the_privacy_choice() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob = Peer::enroll(&server, "Bob").await;
  let (alice_ui, bob_id) = (backend_of(&alice), bob.client.user_id());
  server.set_last_seen(bob_id, 1_799_999_000_000);

  // The first read has nothing yet and asks the server; the answer is there afterwards.
  assert_eq!(alice_ui.last_seen(bob_id), None);
  run_commands(&mut alice, &alice_ui).await;
  assert_eq!(alice_ui.last_seen(bob_id), Some(1_799_999_000_000));

  // Alice's own choice goes to the server and is read back.
  assert_eq!(alice_ui.privacy().last_seen, LastSeenVisibility::Everyone);
  alice_ui.set_last_seen_visibility(LastSeenVisibility::Nobody);
  run_commands(&mut alice, &alice_ui).await;
  assert_eq!(alice_ui.privacy().last_seen, LastSeenVisibility::Nobody);
  assert!(alice_ui.privacy().is_last_seen_available);
}

#[tokio::test]
async fn people_going_offline_are_read_back_in_one_batch() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob = Peer::enroll(&server, "Bob").await;
  let carol = Peer::enroll(&server, "Carol").await;
  let alice_ui = backend_of(&alice);
  let (bob_id, carol_id) = (bob.client.user_id(), carol.client.user_id());
  server.set_last_seen(bob_id, 1_799_999_000_000);
  server.set_last_seen(carol_id, 1_799_999_500_000);

  for (seq, user) in [(1, bob_id), (2, carol_id), (3, bob_id)] {
    alice
      .client
      .handle_frame(GatewayServerMessage::Event {
        seq,
        event: GatewayEvent::PresenceUpdate {
          user_id: user,
          state: PresenceState::Offline,
          maybe_status: None,
        },
      })
      .await
      .unwrap();
  }
  // Nothing asked yet: the events only mark them; the runner's tick reads them together.
  assert!(alice.client.refresh_stale_last_seen().await.unwrap());
  assert!(
    !alice.client.refresh_stale_last_seen().await.unwrap(),
    "nothing left"
  );
  assert_eq!(alice_ui.last_seen(bob_id), Some(1_799_999_000_000));
  assert_eq!(alice_ui.last_seen(carol_id), Some(1_799_999_500_000));
}

#[tokio::test]
async fn the_call_history_lists_missed_incoming_and_outgoing_calls() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "team")
    .await
    .unwrap();
  let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
  let call = |raw: i64, started_by, is_missed, participants| CallHistoryEntry {
    call_id: serde_json::from_value(serde_json::json!(raw.to_string())).unwrap(),
    conversation_id: channel,
    maybe_started_by: Some(started_by),
    started_at: UnixMillis(1_799_000_000_000 + raw),
    maybe_ended_at: Some(UnixMillis(1_799_000_060_000 + raw)),
    participants,
    is_missed,
  };
  server.add_call_history(call(1, bob_id, true, vec![bob_id]));
  server.add_call_history(call(2, alice_id, false, vec![alice_id, bob_id]));
  server.add_call_history(call(3, bob_id, false, vec![alice_id, bob_id]));

  let alice_ui = backend_of(&alice);
  assert!(
    alice_ui.call_history().is_empty(),
    "the first look asks the server"
  );
  run_commands(&mut alice, &alice_ui).await;
  let calls = alice_ui.call_history();
  let directions: Vec<_> = calls.iter().map(|call| call.direction).collect();
  // Newest first: 3 (incoming), 2 (outgoing), 1 (missed: Alice was rung but never joined).
  assert_eq!(
    directions,
    [
      CallDirection::Incoming,
      CallDirection::Outgoing,
      CallDirection::Missed
    ]
  );
  assert_eq!(calls[0].duration_ms(), Some(60_000));
  assert_eq!(calls[0].conversation, channel);
}
