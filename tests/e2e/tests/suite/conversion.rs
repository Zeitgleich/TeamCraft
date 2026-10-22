//! Converting a channel between private and public: who may, who can join afterwards, what the
//! newcomers read, and what members are told.
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::peer::settle;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn converting_a_channel_changes_who_can_join_and_tells_the_members() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let mut erin = Peer::enroll(&stack, "erin").await;
  let dave = Peer::enroll(&stack, "dave").await;
  let space = space_with(
    &stack,
    &alice,
    "Project A",
    &[(&carol, "member"), (&erin, "member"), (&dave, "guest")],
  )
  .await;
  let channel = channel_with(&mut alice, space, "plans", &mut [&mut carol]).await;
  alice
    .client
    .send_text(channel, None, "before the conversion")
    .await
    .unwrap();
  carol.wait_for_text(channel, "before the conversion").await;
  let visibility = format!("/conversations/{channel}/visibility");
  let join = format!("/conversations/{channel}/join");

  // A private channel is invisible and closed to a space member who isn't in it.
  let hidden = stack
    .get(&erin.tokens, &format!("/conversations/{channel}"))
    .await;
  assert_eq!(hidden.status, StatusCode::NOT_FOUND, "{hidden:?}");
  let closed = stack.post(&erin.tokens, &join, json!({})).await;
  assert!(!closed.status.is_success(), "{closed:?}");

  // Plain members can't convert; unknown values are refused.
  let by_member = stack
    .put(&carol.tokens, &visibility, json!({"visibility": "public"}))
    .await;
  assert_eq!(by_member.status, StatusCode::FORBIDDEN, "{by_member:?}");
  let unknown = stack
    .put(&alice.tokens, &visibility, json!({"visibility": "secretish"}))
    .await;
  assert_eq!(unknown.status, StatusCode::BAD_REQUEST, "{unknown:?}");

  // The channel admin makes it public; newcomers only read what is written from now on.
  let converted = stack
    .put(
      &alice.tokens,
      &visibility,
      json!({"visibility": "public", "history_policy": "from_join"}),
    )
    .await
    .expect(StatusCode::OK);
  assert_eq!(converted.body["visibility"], "public", "{}", converted.body);
  assert_eq!(
    converted.body["history_policy"], "from_join",
    "{}",
    converted.body
  );
  let browse = stack
    .get(
      &erin.tokens,
      &format!("/conversations?space_id={space}&joinable=true"),
    )
    .await
    .expect(StatusCode::OK);
  assert!(
    browse.body.to_string().contains(&channel.to_string()),
    "{}",
    browse.body
  );
  let by_guest = stack.post(&dave.tokens, &join, json!({})).await;
  assert!(
    matches!(by_guest.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
    "guests never join on their own: {by_guest:?}"
  );
  stack
    .post(&erin.tokens, &join, json!({}))
    .await
    .expect(StatusCode::NO_CONTENT);
  // An existing member brings the newcomer into the encrypted group.
  alice.client.add_member(channel, erin.user_id()).await.unwrap();
  erin.wait_joined(channel).await;
  alice
    .client
    .send_text(channel, None, "after the conversion")
    .await
    .unwrap();
  erin.wait_for_text(channel, "after the conversion").await;
  assert!(!erin.texts(channel).contains(&"before the conversion".to_owned()));

  // Members are told, and the audit log records who did it.
  let events = stack
    .get(&carol.tokens, &format!("/conversations/{channel}/system-events"))
    .await
    .expect(StatusCode::OK);
  assert!(
    events.body.to_string().contains("privacy_changed"),
    "{}",
    events.body
  );
  let audit = stack
    .get(&alice.tokens, "/admin/audit")
    .await
    .expect(StatusCode::OK);
  assert!(
    audit.body.to_string().contains("conversation_visibility_changed"),
    "{}",
    audit.body
  );

  // Back to private: members stay, the door closes again.
  stack
    .put(&alice.tokens, &visibility, json!({"visibility": "private"}))
    .await
    .expect(StatusCode::OK);
  let mut bob = Peer::enroll(&stack, "bob").await;
  stack
    .add_space_member(
      &alice.tokens,
      &space.to_string(),
      &bob.user_id().to_string(),
      "member",
    )
    .await;
  let shut = stack.post(&bob.tokens, &join, json!({})).await;
  assert!(!shut.status.is_success(), "{shut:?}");
  alice
    .client
    .send_text(channel, None, "still private")
    .await
    .unwrap();
  erin.wait_for_text(channel, "still private").await;
  settle(&mut [&mut bob], Duration::from_secs(1)).await;
  assert!(bob.texts(channel).is_empty());

  // Integration channels carry machine output and never become public.
  let machine = alice.client.create_channel(space, "machine-logs").await.unwrap();
  stack
    .patch(
      &alice.tokens,
      &format!("/conversations/{machine}"),
      json!({"is_integration": true}),
    )
    .await
    .expect(StatusCode::OK);
  let public_logs = stack
    .put(
      &alice.tokens,
      &format!("/conversations/{machine}/visibility"),
      json!({"visibility": "public"}),
    )
    .await;
  assert_eq!(public_logs.status, StatusCode::CONFLICT, "{public_logs:?}");

  stack.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_history_in_a_public_channel_needs_an_explicit_choice_by_the_adding_member() {
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
  let channel = channel_with(&mut alice, space, "plans", &mut []).await;
  alice.client.send_text(channel, None, "old news").await.unwrap();

  // The server says "public, newcomers get the full history". It is the server's word, so a
  // member's client doesn't share the past with somebody who may just have joined on their own.
  stack
    .put(
      &alice.tokens,
      &format!("/conversations/{channel}/visibility"),
      json!({"visibility": "public", "history_policy": "full"}),
    )
    .await
    .expect(StatusCode::OK);
  let join = format!("/conversations/{channel}/join");
  stack
    .post(&erin.tokens, &join, json!({}))
    .await
    .expect(StatusCode::NO_CONTENT);
  alice.client.add_member(channel, erin.user_id()).await.unwrap();
  erin.wait_joined(channel).await;
  alice.client.send_text(channel, None, "new news").await.unwrap();
  erin.wait_for_text(channel, "new news").await;
  assert!(
    !erin.texts(channel).contains(&"old news".to_owned()),
    "history leaked to a self-joiner"
  );

  // Full history is possible when the adding member chooses it.
  stack
    .post(&carol.tokens, &join, json!({}))
    .await
    .expect(StatusCode::NO_CONTENT);
  alice
    .client
    .add_member_with_history(
      channel,
      carol.user_id(),
      Some(teamcraft_proto::HistoryPolicy::Full),
    )
    .await
    .unwrap();
  carol.wait_joined(channel).await;
  // With the keys of the past, her client can read what it fetches from before her join.
  carol.client.load_older_messages(channel).await.unwrap();
  carol.wait_for_text(channel, "old news").await;

  // PATCH is the same conversion: integration channels still can't become public, and it is audited.
  let machine = alice.client.create_channel(space, "machine-logs").await.unwrap();
  stack
    .patch(
      &alice.tokens,
      &format!("/conversations/{machine}"),
      json!({"is_integration": true}),
    )
    .await
    .expect(StatusCode::OK);
  let patched = stack
    .patch(
      &alice.tokens,
      &format!("/conversations/{machine}"),
      json!({"visibility": "public"}),
    )
    .await;
  assert_eq!(patched.status, StatusCode::CONFLICT, "{patched:?}");
  stack
    .patch(
      &alice.tokens,
      &format!("/conversations/{channel}"),
      json!({"visibility": "private"}),
    )
    .await
    .expect(StatusCode::OK);
  let audit = stack
    .get(&alice.tokens, "/admin/audit")
    .await
    .expect(StatusCode::OK)
    .body
    .to_string();
  assert!(
    audit.matches("conversation_visibility_changed").count() >= 2,
    "{audit}"
  );

  stack.stop().await;
}
