//! The channel moderator role: removes members, but doesn't manage the channel or its roles.
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::peer::settle;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_channel_moderator_removes_members_but_cannot_manage_roles_or_the_channel() {
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
  let channel = channel_with(&mut alice, space, "team", &mut [&mut carol, &mut erin]).await;
  let carol_id = carol.user_id().to_string();
  let members_path = |user: &str| format!("/conversations/{channel}/members/{user}");

  // Only somebody who manages roles hands out the role; guests can't hold it.
  let by_member = stack
    .patch(
      &erin.tokens,
      &members_path(&carol_id),
      json!({"role": "moderator"}),
    )
    .await;
  assert_eq!(by_member.status, StatusCode::FORBIDDEN, "{by_member:?}");
  let guest = stack
    .patch(
      &alice.tokens,
      &members_path(&dave.user_id().to_string()),
      json!({"role": "moderator"}),
    )
    .await;
  assert!(!guest.status.is_success(), "a guest became moderator: {guest:?}");
  stack
    .patch(
      &alice.tokens,
      &members_path(&carol_id),
      json!({"role": "moderator"}),
    )
    .await
    .expect(StatusCode::NO_CONTENT);
  let listed = stack
    .get(&alice.tokens, &format!("/conversations/{channel}/members"))
    .await
    .expect(StatusCode::OK);
  assert!(
    listed.body.to_string().contains("\"role\":\"moderator\""),
    "{}",
    listed.body
  );

  // The moderator can't touch roles, the channel's properties or the channel admin...
  let promote = stack
    .patch(
      &carol.tokens,
      &members_path(&erin.user_id().to_string()),
      json!({"role": "moderator"}),
    )
    .await;
  assert_eq!(promote.status, StatusCode::FORBIDDEN, "{promote:?}");
  let rename = stack
    .patch(
      &carol.tokens,
      &format!("/conversations/{channel}"),
      json!({"name": "renamed"}),
    )
    .await;
  assert_eq!(rename.status, StatusCode::FORBIDDEN, "{rename:?}");
  let kick_admin = stack
    .delete(&carol.tokens, &members_path(&alice.user_id().to_string()))
    .await;
  assert_eq!(kick_admin.status, StatusCode::FORBIDDEN, "{kick_admin:?}");
  // ...and plain members can't remove anybody.
  let kick_moderator = stack.delete(&erin.tokens, &members_path(&carol_id)).await;
  assert_eq!(kick_moderator.status, StatusCode::FORBIDDEN, "{kick_moderator:?}");

  // The moderator removes Erin, and the keys rotate: Erin reads nothing sent afterwards.
  carol.client.remove_member(channel, erin.user_id()).await.unwrap();
  settle(&mut [&mut alice, &mut carol], Duration::from_secs(2)).await;
  alice
    .client
    .send_text(channel, None, "after the removal")
    .await
    .unwrap();
  carol.wait_for_text(channel, "after the removal").await;
  settle(&mut [&mut erin], Duration::from_secs(2)).await;
  assert!(!erin.texts(channel).contains(&"after the removal".to_owned()));

  stack.stop().await;
}
