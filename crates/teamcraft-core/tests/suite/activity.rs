//! The activity feed, last seen, call history and own devices as the UI reads them: built from
//! decrypted local data and the server's answers, through the same adapter the desktop shell uses.
#![cfg(feature = "ui")]
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::activity::ActivityKind;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::calls::CallDirection;
use teamcraft_client_api::model::thread_id_of;
use teamcraft_client_api::privacy::LastSeenVisibility;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::{CallHistoryEntry, GatewayEvent, GatewayServerMessage, PresenceState, UnixMillis};
use teamcraft_proto::{ConversationUnread, SetReadPositionRequest, SetThreadReadRequest, UnreadResponse};

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
  server.set_unread(alice.client.user_id(), unread_counts(channel, 2, 1));
  alice.client.full_sync().await.unwrap();

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

  // "Mark all as read" moves the read positions of the targets: the conversation's and the thread's.
  alice_ui.mark_activity_read();
  run_commands(&mut alice, &alice_ui).await;
  alice.client.full_sync().await.unwrap();
  let read = alice_ui.activity();
  assert_eq!(read.items.len(), 3);
  assert_eq!(read.unread_count, 0);
}

async fn scenario() -> Scenario {
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
  server.set_unread(alice.client.user_id(), unread_counts(channel, 2, 1));
  alice.client.full_sync().await.unwrap();

  Scenario {
    server,
    alice,
    alice_ui,
    channel,
    question,
  }
}

struct Scenario {
  server: FakeServer,
  alice: Peer,
  alice_ui: teamcraft_core::ui_backend::CoreChatBackend<std::sync::Arc<teamcraft_core::store::MemoryStore>>,
  channel: teamcraft_client_api::model::ConversationId,
  question: teamcraft_client_api::model::MessageId,
}

/// The kinds of the unread entries, sorted by name (entries sent in the same millisecond have no
/// fixed order).
/// The server's counts for `conversation`; zero counts are what a read conversation reports.
fn unread_counts(
  conversation: teamcraft_client_api::model::ConversationId,
  unread_count: u32,
  mention_count: u32,
) -> UnreadResponse {
  UnreadResponse {
    conversations: (unread_count > 0 || mention_count > 0)
      .then_some(ConversationUnread {
        conversation_id: conversation,
        unread_count,
        mention_count,
      })
      .into_iter()
      .collect(),
    unread_thread_count: 0,
    thread_mention_count: 0,
    maybe_max_counted: None,
    maybe_counted_through: None,
  }
}

fn unread_kinds(feed: &teamcraft_client_api::activity::ActivityFeed) -> Vec<&'static str> {
  let mut kinds: Vec<&'static str> = feed
    .items
    .iter()
    .filter(|item| item.is_unread)
    .map(|item| match item.kind {
      ActivityKind::Mention => "mention",
      ActivityKind::Reaction { .. } => "reaction",
      ActivityKind::ThreadReply => "reply",
    })
    .collect();
  kinds.sort_unstable();
  kinds
}

fn entry(
  feed: &teamcraft_client_api::activity::ActivityFeed,
  kind: fn(&ActivityKind) -> bool,
) -> &teamcraft_client_api::activity::ActivityItem {
  feed.items.iter().find(|item| kind(&item.kind)).unwrap()
}

#[tokio::test]
async fn opening_an_entry_reads_it_up_to_that_message_and_never_back() {
  let Scenario {
    mut alice,
    alice_ui,
    channel,
    ..
  } = scenario().await;
  let feed = alice_ui.activity();
  let reaction = entry(&feed, |kind| matches!(kind, ActivityKind::Reaction { .. }));
  assert_eq!(unread_kinds(&feed).len(), 3);

  // The reaction is the oldest top-level entry: opening it reads it, not the mention after it.
  alice_ui.mark_read_through(channel, reaction.maybe_read_through.unwrap());
  run_commands(&mut alice, &alice_ui).await;
  let after = alice_ui.activity();
  assert_eq!(unread_kinds(&after), ["mention", "reply"]);
  assert_eq!(after.unread_count, 2);

  // An older position written late doesn't unread it.
  alice_ui.mark_read_through(channel, reaction.maybe_read_through.unwrap());
  run_commands(&mut alice, &alice_ui).await;
  assert_eq!(alice_ui.activity().unread_count, 2);
}

#[tokio::test]
async fn reading_the_thread_directly_reads_its_reply_and_nothing_else() {
  let Scenario {
    mut alice,
    alice_ui,
    channel,
    question,
    ..
  } = scenario().await;

  alice_ui.mark_thread_read(channel, thread_id_of(question));
  run_commands(&mut alice, &alice_ui).await;
  alice.client.refresh_read_state().await.unwrap();

  assert_eq!(unread_kinds(&alice_ui.activity()), ["mention", "reaction"]);
}

#[tokio::test]
async fn reading_the_conversation_reads_the_reaction_and_the_mention() {
  let Scenario {
    mut alice,
    alice_ui,
    channel,
    ..
  } = scenario().await;

  alice_ui.mark_read(channel);
  run_commands(&mut alice, &alice_ui).await;

  // The reply belongs to the thread, whose own position is behind.
  assert_eq!(unread_kinds(&alice_ui.activity()), ["reply"]);
}

#[tokio::test]
async fn a_read_position_from_another_device_reads_the_entries() {
  let Scenario {
    server,
    mut alice,
    alice_ui,
    channel,
    question,
  } = scenario().await;
  let feed = alice_ui.activity();
  let mention = entry(&feed, |kind| *kind == ActivityKind::Mention);
  let reply = entry(&feed, |kind| *kind == ActivityKind::ThreadReply);

  // The other device reads the conversation and the thread: the server keeps the positions.
  let other_device = alice.api.clone();
  other_device
    .set_read_position(
      channel,
      &SetReadPositionRequest {
        message_id: mention.maybe_read_through.unwrap(),
      },
    )
    .await
    .unwrap();
  other_device
    .set_thread_read(
      channel,
      thread_id_of(question),
      &SetThreadReadRequest {
        message_id: reply.maybe_read_through.unwrap(),
      },
    )
    .await
    .unwrap();
  assert_eq!(
    alice_ui.activity().unread_count,
    3,
    "this device hasn't heard yet"
  );

  // The server's counts follow the position (this device gets them as `unread_update`).
  server.set_unread(alice.client.user_id(), unread_counts(channel, 0, 0));
  alice.client.full_sync().await.unwrap();
  assert_eq!(alice_ui.activity().unread_count, 0);
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
async fn people_going_offline_are_read_only_when_shown() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob = Peer::enroll(&server, "Bob").await;
  let carol = Peer::enroll(&server, "Carol").await;
  let alice_ui = backend_of(&alice);
  let (bob_id, carol_id) = (bob.client.user_id(), carol.client.user_id());
  server.set_last_seen(bob_id, 1_799_999_000_000);
  server.set_last_seen(carol_id, 1_799_999_500_000);
  // The connection's own reads are done first.
  let mut upkeep = teamcraft_core::upkeep::Upkeep::new(std::time::Duration::from_secs(60));
  upkeep.tick(&mut alice.client).await;
  let before = server.rest_call_count(alice.client.user_id());

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
  // Issue #252: nobody looks, so nothing is asked.
  upkeep.tick(&mut alice.client).await;
  assert_eq!(server.rest_call_count(alice.client.user_id()), before);

  // Shown: read once, then known.
  assert_eq!(alice_ui.last_seen(bob_id), None);
  run_commands(&mut alice, &alice_ui).await;
  assert_eq!(alice_ui.last_seen(bob_id), Some(1_799_999_000_000));
  assert_eq!(alice_ui.last_seen(carol_id), None, "not shown yet");
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

#[tokio::test]
async fn the_device_settings_list_own_devices_and_sign_one_out() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  alice.client.upload_key_bag().await.unwrap();
  let laptop = Peer::enroll_as(alice.api.clone(), "Alice laptop").await;
  let (phone_id, laptop_id) = (alice.client.device_id(), laptop.client.device_id());

  let alice_ui = backend_of(&alice);
  assert!(
    alice_ui.own_devices().is_empty(),
    "the first look asks the server"
  );
  run_commands(&mut alice, &alice_ui).await;
  let listed: Vec<_> = alice_ui
    .own_devices()
    .iter()
    .map(|device| (device.id, device.is_current))
    .collect();
  assert_eq!(listed, [(phone_id, true), (laptop_id, false)]);

  // Signing out goes through the core: the server forgets the laptop and the list is read again.
  alice_ui.revoke_own_device(laptop_id);
  run_commands(&mut alice, &alice_ui).await;
  let remaining: Vec<_> = alice_ui.own_devices().iter().map(|device| device.id).collect();
  assert_eq!(remaining, [phone_id]);
  assert!(
    server.device_list_of(alice.client.user_id()).is_some(),
    "the signed device list was published without the laptop"
  );
}
