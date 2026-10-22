//! The signed policy's thread options: the auto-follow default for people who never chose, and the
//! threaded view switch the server enforces.
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_core::store::LocalStore;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{Id, ThreadId};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_policy_sets_the_follow_default_and_can_turn_threads_off() {
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
  let channel = channel_with(&mut alice, space, "talk", &mut [&mut carol, &mut erin]).await;
  let settings = |peer: &Peer| {
    let (stack, tokens) = (&stack, peer.tokens.clone());
    async move {
      stack
        .get(&tokens, "/me/notification-settings")
        .await
        .expect(StatusCode::OK)
    }
  };

  // Default: mentioned threads are followed.
  assert_eq!(settings(&carol).await.body["auto_follow_mentioned_threads"], true);

  // The owner turns the default off. Carol never chose, so she follows it; Erin chooses "on" for
  // herself and keeps it whatever the workspace says.
  alice
    .set_policy(|policy| policy.threads.auto_follows_mentioned_threads = false)
    .await;
  assert_eq!(
    settings(&carol).await.body["auto_follow_mentioned_threads"],
    false
  );
  let chosen = stack
    .get(&erin.tokens, "/me/notification-settings")
    .await
    .expect(StatusCode::OK)
    .body;
  let mut chosen = chosen;
  chosen["auto_follow_mentioned_threads"] = json!(true);
  stack
    .put(&erin.tokens, "/me/notification-settings", chosen)
    .await
    .expect(StatusCode::OK);
  assert_eq!(settings(&erin).await.body["auto_follow_mentioned_threads"], true);

  // A reply that mentions both: only Erin is brought into the thread.
  let root = alice.client.send_text(channel, None, "root").await.unwrap();
  let root_id = alice
    .store
    .message(channel, root)
    .unwrap()
    .and_then(|m| m.maybe_server_id)
    .unwrap();
  let thread = ThreadId::from_snowflake(root_id.snowflake());
  alice
    .client
    .send_text_mentioning(
      channel,
      Some(thread),
      "look",
      vec![carol.user_id(), erin.user_id()],
    )
    .await
    .unwrap();
  let follows = |peer: &Peer| {
    let (stack, tokens) = (&stack, peer.tokens.clone());
    async move {
      stack
        .get(&tokens, "/threads")
        .await
        .expect(StatusCode::OK)
        .body
        .to_string()
    }
  };
  assert!(
    follows(&erin).await.contains(&channel.to_string()),
    "Erin chose to follow"
  );
  assert!(
    !follows(&carol).await.contains(&channel.to_string()),
    "Carol follows the workspace default"
  );

  // The owner turns threads off: top-level messages still work, thread replies are refused.
  alice
    .set_policy(|policy| policy.threads.is_threaded_view_enabled = false)
    .await;
  let reply = stack
    .post(
      &carol.tokens,
      &format!("/conversations/{channel}/messages"),
      json!({
        "client_message_id": "00000000000000000000000000000001",
        "device_id": carol.client.device_id().to_string(),
        "content_key_id": "a2V5",
        "ciphertext": "Y2lwaGVydGV4dA==",
        "thread_id": thread.to_string(),
      }),
    )
    .await;
  assert_eq!(reply.status, StatusCode::FORBIDDEN, "{reply:?}");
  carol
    .client
    .send_text(channel, None, "flat is fine")
    .await
    .unwrap();
  alice.wait_for_text(channel, "flat is fine").await;

  stack.stop().await;
}
