//! The "jump to reaction" button's state as the UI reads it: reactions of others to the user's own
//! messages that the user hasn't seen, kept as reactions arrive and synced across devices
//! (notifications.md, "Reactions to my messages").
#![cfg(feature = "ui")]
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{ConversationId, MessageId, UserId};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::MemoryStore;
use teamcraft_core::ui_backend::CoreChatBackend;
use teamcraft_proto::{ListMessagesQuery, MarkReactionsSeenRequest};

type Ui = CoreChatBackend<std::sync::Arc<MemoryStore>>;

#[tokio::test]
async fn only_others_reactions_to_my_messages_count_until_seen() {
  let Scenario {
    mut alice,
    alice_ui,
    mut bob,
    bob_ui,
    channel,
    release,
    question,
    bobs_note,
    ..
  } = scenario().await;
  bob_ui.toggle_reaction(channel, release, "🎉");
  bob_ui.toggle_reaction(channel, release, "👍");
  bob_ui.toggle_reaction(channel, question, "👀");
  // Neither Bob's reaction to his own message nor Alice's to hers counts for Alice.
  bob_ui.toggle_reaction(channel, bobs_note, "🎉");
  alice_ui.toggle_reaction(channel, release, "❤️");
  run_commands(&mut bob, &bob_ui).await;
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let unseen = alice_ui.unseen_reactions(channel);
  assert_eq!(unseen.count, 3);
  assert_eq!(unseen.messages, vec![release, question], "oldest first");
  assert_eq!(bob_ui.unseen_reactions(channel).count, 0);

  // The oldest came into view: it counts down at once, and seen stays seen.
  alice_ui.mark_reactions_seen(channel, vec![release]);
  assert_eq!(alice_ui.unseen_reactions(channel).messages, vec![question]);
  assert_eq!(alice_ui.unseen_reactions(channel).count, 1);
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  alice.client.full_sync().await.unwrap();
  assert_eq!(alice_ui.unseen_reactions(channel).count, 1);

  // A new reaction to a seen message is new again.
  bob_ui.toggle_reaction(channel, release, "🚀");
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(
    alice_ui.unseen_reactions(channel).messages,
    vec![release, question]
  );
}

#[tokio::test]
async fn a_reaction_taken_back_before_it_was_seen_no_longer_counts() {
  let Scenario {
    mut alice,
    alice_ui,
    mut bob,
    bob_ui,
    channel,
    release,
    ..
  } = scenario().await;
  bob_ui.toggle_reaction(channel, release, "🎉");
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(alice_ui.unseen_reactions(channel).count, 1);

  bob_ui.toggle_reaction(channel, release, "🎉");
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let unseen = alice_ui.unseen_reactions(channel);
  assert_eq!(unseen.count, 0);
  assert!(unseen.messages.is_empty());
}

#[tokio::test]
async fn reactions_seen_on_another_device_are_seen_here_and_offline_counts_stay_local() {
  let Scenario {
    server,
    mut alice,
    alice_ui,
    mut bob,
    bob_ui,
    channel,
    release,
    question,
    ..
  } = scenario().await;
  bob_ui.toggle_reaction(channel, release, "🎉");
  bob_ui.toggle_reaction(channel, question, "👀");
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(alice_ui.unseen_reactions(channel).count, 2);

  // Alice's phone saw the reaction to the release: it tells the server, which tells this device.
  let reactions = bobs_reactions(&alice, bob.client.user_id(), channel).await;
  let phone = alice.api.clone();
  phone
    .mark_reactions_seen(
      channel,
      &MarkReactionsSeenRequest {
        message_ids: vec![reactions[0]],
      },
    )
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(alice_ui.unseen_reactions(channel).messages, vec![question]);

  // Offline, this device's own marks count at once and wait in the queue.
  server.set_offline(alice.client.user_id(), true);
  alice_ui.mark_reactions_seen(channel, vec![question]);
  run_commands(&mut alice, &alice_ui).await;
  assert_eq!(alice_ui.unseen_reactions(channel).count, 0);
  server.set_offline(alice.client.user_id(), false);
  alice.client.flush_pending_actions().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(alice_ui.unseen_reactions(channel).count, 0);
}

/// Alice posted two messages and Bob one, in a channel of both.
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
  bob_ui.post_message(channel, None, "a note of mine");
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let seen = bob_ui.messages(channel, 0, 10);
  let id_of = |body: &str| seen.iter().find(|message| message.body == body).unwrap().id;
  Scenario {
    release: id_of("ship the release"),
    question: id_of("what do you think?"),
    bobs_note: id_of("a note of mine"),
    server,
    alice,
    alice_ui,
    bob,
    bob_ui,
    channel,
  }
}

struct Scenario {
  server: FakeServer,
  alice: Peer,
  alice_ui: Ui,
  bob: Peer,
  bob_ui: Ui,
  channel: ConversationId,
  release: MessageId,
  question: MessageId,
  bobs_note: MessageId,
}

/// Bob's messages after his note (his reactions), oldest first, as the server lists them.
async fn bobs_reactions(alice: &Peer, bob: UserId, channel: ConversationId) -> Vec<MessageId> {
  let page = alice
    .api
    .list_messages(channel, &ListMessagesQuery::default())
    .await
    .unwrap();
  let mut ids: Vec<MessageId> = page
    .messages
    .iter()
    .filter(|message| message.sender_user_id == bob)
    .map(|message| message.id)
    .collect();
  ids.sort_unstable();
  // The first is his note.
  ids.remove(0);
  ids
}
