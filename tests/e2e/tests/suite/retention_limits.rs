//! The retention limits of the signed policy: owners of channels and spaces stay within them, also
//! when the limit is lowered after they chose.
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::RetentionSetting;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retention_stays_within_the_workspace_limit_even_after_the_limit_is_lowered() {
  let Some(stack) = Stack::builder().with_fast_retention().start().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let space = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
  let channel = channel_with(&mut alice, space, "chatter", &mut [&mut carol]).await;
  let alice_tokens = alice.tokens.clone();
  let path = format!("/conversations/{channel}/retention");
  let set = |setting: serde_json::Value| {
    let (stack, tokens, path) = (&stack, alice_tokens.clone(), path.clone());
    async move { stack.put(&tokens, &path, json!({"retention": setting})).await }
  };

  // Limit: at most 30 days; the default is 14. A channel may pick anything up to 30 days.
  alice
    .set_policy(|policy| {
      policy.retention.limit = RetentionSetting {
        maybe_max_age_days: Some(30),
        maybe_max_messages: None,
      };
      policy.retention.default = RetentionSetting {
        maybe_max_age_days: Some(14),
        maybe_max_messages: None,
      };
    })
    .await;
  let initial = stack.get(&alice.tokens, &path).await.expect(StatusCode::OK);
  assert_eq!(initial.body["effective"]["max_age_days"], 14, "{}", initial.body);
  assert_eq!(initial.body["limit"]["max_age_days"], 30, "{}", initial.body);
  let too_long = set(json!({"max_age_days": 90})).await;
  assert_eq!(too_long.status, StatusCode::BAD_REQUEST, "{too_long:?}");
  let in_the_limit = set(json!({"max_age_days": 30})).await.expect(StatusCode::OK);
  assert_eq!(in_the_limit.body["effective"]["max_age_days"], 30);
  // Space owners are bound by it too.
  let space_too_long = stack
    .put(
      &alice.tokens,
      &format!("/spaces/{space}/retention"),
      json!({"retention": {"max_age_days": 365}}),
    )
    .await;
  assert_eq!(
    space_too_long.status,
    StatusCode::BAD_REQUEST,
    "{space_too_long:?}"
  );
  // A policy whose default is beyond its own limit is refused.
  let inconsistent = alice
    .client
    .publish_policy({
      let mut policy = alice.client.workspace().unwrap().policy;
      policy.retention.default.maybe_max_age_days = Some(60);
      policy
    })
    .await;
  assert!(
    inconsistent.is_err(),
    "the server accepted a default beyond the limit"
  );

  // The channel keeps 10 messages (own setting), then the limit drops to 3 messages: the stored
  // setting stays, the effective one is cut down, and the job deletes accordingly.
  set(json!({"max_age_days": 30, "max_messages": 10}))
    .await
    .expect(StatusCode::OK);
  for index in 1..=6 {
    alice
      .client
      .send_text(channel, None, &format!("message {index}"))
      .await
      .unwrap();
  }
  let count = || {
    let (stack, tokens) = (&stack, alice_tokens.clone());
    let path = format!("/conversations/{channel}/messages?limit=100");
    async move {
      let stored = stack.get(&tokens, &path).await.expect(StatusCode::OK);
      stored.body["messages"].as_array().unwrap().len()
    }
  };
  // (The stored messages include the client's own bookkeeping.)
  assert!(
    count().await >= 6,
    "nothing is deleted within the channel's own setting"
  );

  alice
    .set_policy(|policy| {
      policy.retention.limit.maybe_max_messages = Some(3);
      policy.retention.default.maybe_max_messages = Some(3);
    })
    .await;
  let lowered = stack.get(&alice.tokens, &path).await.expect(StatusCode::OK);
  assert_eq!(
    lowered.body["own"]["max_messages"], 10,
    "the stored setting stays: {}",
    lowered.body
  );
  assert_eq!(lowered.body["effective"]["max_messages"], 3, "{}", lowered.body);
  let deadline = Instant::now() + Duration::from_secs(30);
  while count().await != 3 {
    assert!(
      Instant::now() < deadline,
      "the job kept more than the limit allows"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
  }

  stack.stop().await;
}
