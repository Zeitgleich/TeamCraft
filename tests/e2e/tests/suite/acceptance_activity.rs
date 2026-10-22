//! v1 acceptance, PLAN.md §5 activity feed and §4 last seen: the feed is built by the client from
//! decrypted messages (the server can't), and last seen follows the privacy choice, appearing
//! offline and the admin switch.
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_client_api::activity::ActivityKind;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::clock::SystemClock;
use teamcraft_core::store::LocalStore;
use teamcraft_core::ui_backend::CoreChatBackend;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{Id, ThreadId};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mentions_reactions_and_thread_replies_reach_the_activity_feed_of_the_person_concerned() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let space = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
  let channel = channel_with(&mut alice, space, "dev", &mut [&mut carol]).await;

  // Alice posts and starts a thread; Carol mentions her, reacts and replies.
  let question = alice
    .client
    .send_text(channel, None, "design question")
    .await
    .unwrap();
  let question_server = alice
    .store
    .message(channel, question)
    .unwrap()
    .and_then(|message| message.maybe_server_id)
    .unwrap();
  let thread = ThreadId::from_snowflake(question_server.snowflake());
  carol.wait_for_text(channel, "design question").await;
  carol
    .client
    .send_text_mentioning(channel, None, "@alice please look", vec![alice.user_id()])
    .await
    .unwrap();
  carol.client.react(channel, question, "👍", true).await.unwrap();
  carol
    .client
    .send_text(channel, Some(thread), "my answer")
    .await
    .unwrap();

  alice
    .wait_until("the feed has the three entries", |peer| {
      backend_of(peer).activity().items.len() == 3
    })
    .await;
  let alice_ui = backend_of(&alice);
  let feed = alice_ui.activity();
  let has = |matching: fn(&ActivityKind) -> bool| feed.items.iter().any(|item| matching(&item.kind));
  assert!(has(|kind| *kind == ActivityKind::Mention), "{feed:?}");
  assert!(has(
    |kind| matches!(kind, ActivityKind::Reaction { emoji } if emoji == "👍")
  ));
  assert!(has(|kind| *kind == ActivityKind::ThreadReply));
  assert_eq!(feed.unread_count, 3);

  // What Carol did herself is nobody's activity, and the server learned nothing of the feed.
  assert!(backend_of(&carol).activity().items.is_empty());

  // Reading moves the account-data mark; the feed keeps its entries but none is unread.
  alice_ui.mark_activity_read();
  for command in alice_ui.drain_commands() {
    alice.client.run_ui_command(command).await.unwrap();
  }
  let read = backend_of(&alice).activity();
  assert_eq!((read.items.len(), read.unread_count), (3, 0));
  stack.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn last_seen_follows_the_privacy_choice_appearing_offline_and_the_admin_switch() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let carol = Peer::enroll(&stack, "carol").await;
  let carol_id = carol.user_id().to_string();
  let carol_tokens = carol.tokens.clone();
  let alice_tokens = alice.tokens.clone();
  let presence_of_carol = || async {
    let reply = stack
      .get(&alice_tokens, &format!("/presence?user_ids={carol_id}"))
      .await
      .expect(StatusCode::OK);
    reply.body["presences"][0].clone()
  };

  // Online: no last seen. After the last device goes, the time is recorded and shown.
  assert!(presence_of_carol().await.get("last_seen").is_none());
  drop(carol);
  let shown = wait_for_last_seen(&presence_of_carol).await;
  assert_eq!(shown["state"], "offline", "{shown}");

  // Hidden by the person, then shown again.
  stack
    .put(&carol_tokens, "/me/privacy", json!({"last_seen": "nobody"}))
    .await
    .expect(StatusCode::OK);
  assert!(presence_of_carol().await.get("last_seen").is_none());
  stack
    .put(&carol_tokens, "/me/privacy", json!({"last_seen": "everyone"}))
    .await
    .expect(StatusCode::OK);
  assert!(presence_of_carol().await.get("last_seen").is_some());

  // Appearing offline hides it too.
  stack
    .put(&carol_tokens, "/me/presence", json!({"manual": "invisible"}))
    .await
    .expect(StatusCode::OK);
  assert!(presence_of_carol().await.get("last_seen").is_none());
  stack
    .put(&carol_tokens, "/me/presence", json!({"manual": "auto"}))
    .await
    .expect(StatusCode::OK);

  // The owner switches last seen off for the whole workspace.
  alice
    .set_policy(|policy| policy.presence.is_last_seen_enabled = false)
    .await;
  assert!(presence_of_carol().await.get("last_seen").is_none());
  let privacy = stack
    .get(&carol_tokens, "/me/privacy")
    .await
    .expect(StatusCode::OK);
  assert_eq!(privacy.body["is_last_seen_available"], false, "{}", privacy.body);
  stack.stop().await;
}

fn backend_of(peer: &Peer) -> CoreChatBackend<std::sync::Arc<teamcraft_core::store::MemoryStore>> {
  CoreChatBackend::new(peer.store.clone(), peer.user_id(), Box::new(SystemClock))
}

/// The disconnect is processed in a background task: wait until it was recorded.
async fn wait_for_last_seen<F, Fut>(presence: &F) -> Value
where
  F: Fn() -> Fut,
  Fut: std::future::Future<Output = Value>,
{
  let deadline = Instant::now() + Duration::from_secs(15);
  loop {
    let entry = presence().await;
    if entry.get("last_seen").is_some() {
      return entry;
    }
    assert!(Instant::now() < deadline, "no last_seen after the device left");
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}
