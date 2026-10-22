//! v1 acceptance, PLAN.md §3 Threads: replies, auto-follow, the threads inbox, unread and mention
//! counts, manual follow and read positions.
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_core::store::LocalStore;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{Id, ThreadId};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn threads_follow_their_participants_and_feed_the_inbox_and_unread_counts() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let mut erin = Peer::enroll(&stack, "erin").await;
  let space = space_with(
    &stack,
    &alice,
    "Project A",
    &[(&carol, "member"), (&erin, "member")],
  )
  .await;
  let channel = channel_with(&mut alice, space, "dev", &mut [&mut carol, &mut erin]).await;

  // Alice starts a thread; Carol replies, mentioning Erin.
  let root = alice
    .client
    .send_text(channel, None, "design question")
    .await
    .unwrap();
  let root_server = alice
    .store
    .message(channel, root)
    .unwrap()
    .and_then(|m| m.maybe_server_id)
    .unwrap();
  let thread = ThreadId::from_snowflake(root_server.snowflake());
  carol.wait_for_text(channel, "design question").await;
  carol
    .client
    .send_text_mentioning(
      channel,
      Some(thread),
      "have you seen this, Erin?",
      vec![erin.user_id()],
    )
    .await
    .unwrap();

  // The root's author and the mentioned member follow without doing anything; Carol follows by
  // replying. The inbox shows unread replies and the mention.
  let inbox = |peer: &Peer| {
    let (stack, tokens) = (&stack, peer.tokens.clone());
    async move { stack.get(&tokens, "/threads").await.expect(StatusCode::OK).body }
  };
  let alice_inbox = inbox(&alice).await;
  let entry = &alice_inbox["threads"][0];
  assert_eq!(entry["thread_id"], thread.to_string(), "{alice_inbox}");
  assert_eq!(entry["follow_reason"], "started");
  assert_eq!(entry["unread_count"], 1);
  assert_eq!(entry["reply_count"], 1);
  let erin_inbox = inbox(&erin).await;
  assert_eq!(
    erin_inbox["threads"][0]["follow_reason"], "mentioned",
    "{erin_inbox}"
  );
  assert_eq!(erin_inbox["threads"][0]["mention_count"], 1);
  let carol_inbox = inbox(&carol).await;
  assert_eq!(
    carol_inbox["threads"][0]["unread_count"], 0,
    "own reply isn't unread: {carol_inbox}"
  );

  // Unread totals: replies count in the threads, not the channel; Erin has a thread mention.
  let unread = stack
    .get(&erin.tokens, "/unread")
    .await
    .expect(StatusCode::OK)
    .body;
  assert_eq!(unread["unread_thread_count"], 1, "{unread}");
  assert_eq!(unread["thread_mention_count"], 1, "{unread}");

  // Reading the thread clears the counts; `unread_only` then hides it.
  let newest = erin_inbox["threads"][0]["last_reply"]["id"]
    .as_str()
    .unwrap()
    .to_owned();
  stack
    .put(
      &erin.tokens,
      &format!("/conversations/{channel}/threads/{thread}/read"),
      json!({"message_id": newest}),
    )
    .await
    .expect(StatusCode::NO_CONTENT);
  let unread = stack
    .get(&erin.tokens, "/unread")
    .await
    .expect(StatusCode::OK)
    .body;
  assert_eq!(unread["unread_thread_count"], 0, "{unread}");
  let only_unread = stack
    .get(&erin.tokens, "/threads?unread_only=true")
    .await
    .expect(StatusCode::OK)
    .body;
  assert!(
    only_unread["threads"].as_array().unwrap().is_empty(),
    "{only_unread}"
  );

  // Unfollowing sticks: later replies don't bring Erin back, her own reply does.
  let follow = format!("/conversations/{channel}/threads/{thread}/follow");
  stack
    .delete(&erin.tokens, &follow)
    .await
    .expect(StatusCode::NO_CONTENT);
  carol
    .client
    .send_text(channel, Some(thread), "another reply")
    .await
    .unwrap();
  assert!(inbox(&erin).await["threads"].as_array().unwrap().is_empty());
  stack
    .put(&erin.tokens, &follow, json!({}))
    .await
    .expect(StatusCode::NO_CONTENT);
  assert_eq!(inbox(&erin).await["threads"][0]["follow_reason"], "manual");

  stack.stop().await;
}
