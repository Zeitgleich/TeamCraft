//! Administration without content access: deactivating a user ends their access, and the retention
//! job deletes old ciphertext.

use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deactivating_a_user_ends_access_and_reactivating_restores_it() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let space = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
  let channel = channel_with(&mut alice, space, "general", &mut [&mut carol]).await;
  let carol_id = carol.user_id().to_string();

  // Members can't deactivate anybody.
  let by_member = stack
    .post(
      &carol.tokens,
      &format!("/admin/users/{carol_id}/deactivate"),
      json!({}),
    )
    .await;
  assert_eq!(by_member.status, StatusCode::FORBIDDEN, "{by_member:?}");

  stack
    .post(
      &alice.tokens,
      &format!("/admin/users/{carol_id}/deactivate"),
      json!({}),
    )
    .await
    .expect(StatusCode::NO_CONTENT);
  // Her still valid token is refused now.
  let refused = stack.get(&carol.tokens, "/me").await;
  assert!(
    matches!(refused.status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN),
    "{refused:?}"
  );
  // The account is suspended, not removed (#666): her devices stay, so a reactivation restores
  // everything, but nothing reaches her any more.
  let active_devices = || async {
    let devices = stack
      .get(&alice.tokens, &format!("/admin/users/{carol_id}/devices"))
      .await
      .expect(StatusCode::OK);
    let listed = devices.body["devices"].as_array().unwrap();
    assert!(!listed.is_empty(), "{}", devices.body);
    listed
      .iter()
      .filter(|device| device["is_revoked"] != true)
      .count()
  };
  assert_eq!(active_devices().await, 1);
  // The server closes her gateway connection...
  let deadline = Instant::now() + Duration::from_secs(10);
  while carol.client.pump(&mut carol.gateway).await.is_ok() {
    assert!(Instant::now() < deadline, "her gateway connection stayed open");
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  // ...so she doesn't get what is sent from now on.
  alice
    .client
    .send_text(channel, None, "after deactivation")
    .await
    .unwrap();
  assert!(!carol.texts(channel).contains(&"after deactivation".to_owned()));

  // The action is in the audit log (who and what, never content).
  let audit = stack
    .get(&alice.tokens, "/admin/audit")
    .await
    .expect(StatusCode::OK);
  assert!(audit.body.to_string().contains("deactivate"), "{}", audit.body);

  stack
    .post(
      &alice.tokens,
      &format!("/admin/users/{carol_id}/reactivate"),
      json!({}),
    )
    .await
    .expect(StatusCode::NO_CONTENT);
  stack.get(&carol.tokens, "/me").await.expect(StatusCode::OK);
  assert_eq!(active_devices().await, 1);

  stack.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_retention_job_deletes_messages_beyond_the_channels_limit() {
  let Some(stack) = Stack::builder().with_fast_retention().start().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let space = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
  let channel = channel_with(&mut alice, space, "chatter", &mut [&mut carol]).await;

  let retention = stack
    .put(
      &alice.tokens,
      &format!("/conversations/{channel}/retention"),
      json!({"retention": {"max_messages": 3}}),
    )
    .await
    .expect(StatusCode::OK);
  assert_eq!(
    retention.body["effective"]["max_messages"], 3,
    "{}",
    retention.body
  );
  // Members can't change it.
  let by_member = stack
    .put(
      &carol.tokens,
      &format!("/conversations/{channel}/retention"),
      json!({"retention": {"max_messages": 1}}),
    )
    .await;
  assert!(!by_member.status.is_success(), "{by_member:?}");

  for index in 1..=6 {
    alice
      .client
      .send_text(channel, None, &format!("message {index}"))
      .await
      .unwrap();
  }
  // The job runs every second; only the newest three ciphertexts stay on the server.
  let deadline = Instant::now() + Duration::from_secs(30);
  loop {
    let stored = stack
      .get(
        &alice.tokens,
        &format!("/conversations/{channel}/messages?limit=100"),
      )
      .await
      .expect(StatusCode::OK);
    let count = stored.body["messages"].as_array().unwrap().len();
    if count == 3 {
      break;
    }
    assert!(
      Instant::now() < deadline,
      "retention left {count} messages on the server"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
  }

  stack.stop().await;
}
