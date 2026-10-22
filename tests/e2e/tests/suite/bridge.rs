//! The integration bridge as a separate process: webhooks and log lines arrive in an encrypted
//! integration channel, decrypted by a member.

use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::bridge::{BridgeProcess, SOURCE_TOKEN};
use teamcraft_e2e::scenario::space_with;
use teamcraft_e2e::{Peer, Stack};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn webhooks_and_logs_arrive_decrypted_in_an_integration_channel() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;

  // An admin registers the bot; the token is shown once.
  let bot = stack
    .post(&alice.tokens, "/admin/bots", json!({"name": "NAS bridge"}))
    .await
    .expect(StatusCode::CREATED);
  let bot_token = bot.body["token"].as_str().unwrap().to_owned();
  let bot_id = bot.body["bot"]["id"].as_str().unwrap().to_owned();

  // The integration channel: flagged by an admin, the bot is a guest of the space.
  let space = space_with(&stack, &alice, "Ops", &[(&carol, "member")]).await;
  stack
    .add_space_member(&alice.tokens, &space.to_string(), &bot_id, "guest")
    .await;
  let channel = alice.client.create_channel(space, "nas-logs").await.unwrap();
  stack
    .patch(
      &alice.tokens,
      &format!("/conversations/{channel}"),
      json!({"is_integration": true}),
    )
    .await
    .expect(StatusCode::OK);

  // The bridge enrolls the bot's device; then a member adds the bot to the channel.
  let bridge = BridgeProcess::start(
    &stack,
    &bot_token,
    &channel.to_string(),
    &carol.user_id().to_string(),
  )
  .await;
  let bot_user = bot_id.parse().unwrap();
  let deadline = Instant::now() + Duration::from_secs(60);
  while alice.client.add_member(channel, bot_user).await.is_err() {
    assert!(
      Instant::now() < deadline,
      "the bot never became addable:\n{}",
      bridge.service.tail(30)
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
  }
  alice.client.add_member(channel, carol.user_id()).await.unwrap();
  carol.wait_joined(channel).await;

  let status = bridge
    .post(
      &format!("/hooks/{SOURCE_TOKEN}"),
      "application/json",
      r#"{"text":"Deploy <https://ci.example.com/42|#42> finished","username":"ci"}"#,
    )
    .await;
  assert_eq!(status, StatusCode::OK, "{}", bridge.service.tail(30));
  carol
    .wait_for_text(
      channel,
      "**ci**\nDeploy [#42](https://ci.example.com/42) finished",
    )
    .await;

  let status = bridge
    .post(
      &format!("/ingest/{SOURCE_TOKEN}"),
      "text/plain",
      "INFO started\nWARN disk at 91%\nINFO done\n",
    )
    .await;
  assert_eq!(status, StatusCode::OK);
  carol
    .wait_for_text(channel, "```\nINFO started\nWARN disk at 91%\nINFO done\n```")
    .await;

  // Wrong tokens and empty payloads are refused without reaching the channel.
  assert_eq!(
    bridge
      .post("/hooks/wrong-token", "application/json", r#"{"text":"x"}"#)
      .await,
    StatusCode::NOT_FOUND
  );
  assert_eq!(
    bridge
      .post(&format!("/hooks/{SOURCE_TOKEN}"), "application/json", "{}")
      .await,
    StatusCode::BAD_REQUEST
  );

  // The server holds ciphertext only, all of it from the bot.
  let stored = stack
    .get(
      &alice.tokens,
      &format!("/conversations/{channel}/messages?limit=100"),
    )
    .await
    .expect(StatusCode::OK);
  let raw = stored.body.to_string();
  for secret in ["Deploy", "disk at 91%"] {
    assert!(!raw.contains(secret), "{secret:?} is visible to the server");
  }
  // Besides the bot's two messages the channel holds the encrypted system messages the members'
  // clients post (channel created, member added).
  let from_bot = stored.body["messages"]
    .as_array()
    .unwrap()
    .iter()
    .filter(|message| message["sender_user_id"] == bot_id.as_str())
    .count();
  assert_eq!(from_bot, 2, "{raw}");

  stack.stop().await;
}
