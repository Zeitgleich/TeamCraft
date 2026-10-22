//! v1 acceptance, PLAN.md §1 spaces: moving a channel between spaces keeps its encryption group,
//! muting a whole space is inherited by its channels, and channel owners set their own retention
//! within the workspace limit. Space-scoped search is covered by the core's search tests.
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::RetentionSetting;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moved_channel_keeps_working_and_a_muted_space_is_inherited() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let source = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
  let target = space_with(&stack, &alice, "Project B", &[]).await;
  let channel = channel_with(&mut alice, source, "plans", &mut [&mut carol]).await;
  alice
    .client
    .send_text(channel, None, "before the move")
    .await
    .unwrap();
  carol.wait_for_text(channel, "before the move").await;

  // Muting the space is carol's own setting; the channel follows it until she configures the channel.
  let mute = format!("/spaces/{source}/notification-settings");
  stack
    .put(&carol.tokens, &mute, json!({"level": "none"}))
    .await
    .expect(StatusCode::OK);
  let conversation = stack
    .get(&carol.tokens, &format!("/conversations/{channel}"))
    .await
    .expect(StatusCode::OK);
  assert_eq!(
    conversation.body["my_membership"]["is_notification_inherited"], true,
    "{}",
    conversation.body
  );
  let spaces = stack.get(&carol.tokens, "/spaces").await.expect(StatusCode::OK);
  assert!(
    spaces
      .body
      .to_string()
      .contains(r#""my_notifications":{"level":"none"}"#),
    "{}",
    spaces.body
  );

  // Only space admins of both spaces move channels.
  let move_path = format!("/conversations/{channel}/space");
  let by_member = stack
    .put(&carol.tokens, &move_path, json!({"space_id": target.to_string()}))
    .await;
  assert_eq!(by_member.status, StatusCode::FORBIDDEN, "{by_member:?}");
  let moved = stack
    .put(&alice.tokens, &move_path, json!({"space_id": target.to_string()}))
    .await
    .expect(StatusCode::OK);
  assert_eq!(moved.body["space_id"], target.to_string(), "{}", moved.body);

  // Carol is no member of Project B but keeps the channel, and the group still works.
  alice
    .client
    .send_text(channel, None, "after the move")
    .await
    .unwrap();
  carol.wait_for_text(channel, "after the move").await;
  carol
    .client
    .send_text(channel, None, "carol answers")
    .await
    .unwrap();
  alice.wait_for_text(channel, "carol answers").await;
  let events = stack
    .get(&carol.tokens, &format!("/conversations/{channel}/system-events"))
    .await
    .expect(StatusCode::OK);
  assert!(
    events.body.to_string().contains(r#""kind":"moved""#),
    "{}",
    events.body
  );
  let audit = stack
    .get(&alice.tokens, "/admin/audit?action=channel_moved")
    .await
    .expect(StatusCode::OK);
  assert_eq!(
    audit.body["entries"].as_array().map(Vec::len),
    Some(1),
    "{}",
    audit.body
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_channel_owner_sets_retention_within_the_limit() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let mut dave = Peer::enroll(&stack, "dave").await;
  let space = space_with(
    &stack,
    &alice,
    "Project A",
    &[(&carol, "member"), (&dave, "member")],
  )
  .await;
  alice
    .set_policy(|policy| {
      policy.retention.default = RetentionSetting {
        maybe_max_age_days: Some(30),
        maybe_max_messages: None,
      };
      policy.retention.limit = RetentionSetting {
        maybe_max_age_days: Some(90),
        maybe_max_messages: None,
      };
    })
    .await;
  // Carol founded the channel, so she owns it; she is not an administrator.
  let channel = channel_with(&mut carol, space, "notes", &mut [&mut dave]).await;
  let path = format!("/conversations/{channel}/retention");

  let own = stack
    .put(&carol.tokens, &path, json!({"retention": {"max_age_days": 14}}))
    .await
    .expect(StatusCode::OK);
  assert_eq!(own.body["effective"]["max_age_days"], 14, "{}", own.body);
  let too_long = stack
    .put(&carol.tokens, &path, json!({"retention": {"max_age_days": 365}}))
    .await;
  assert_eq!(too_long.status, StatusCode::BAD_REQUEST, "{too_long:?}");
  let by_member = stack
    .put(&dave.tokens, &path, json!({"retention": {"max_age_days": 7}}))
    .await;
  assert_eq!(by_member.status, StatusCode::FORBIDDEN, "{by_member:?}");
}
