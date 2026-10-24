//! Issue #317, finding spaces and bringing people in: a colleague finds an open space in the list
//! (with its member count), joins it in one step and reads its channel; someone picked from the
//! directory gets in the same way. Space membership is directory data the server keeps; the
//! channel's encryption group only grows through a member's signed add (e2ee.md §4.6).
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use teamcraft_core::client::workspace_admin::ChannelOptions;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::SpaceId;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_space_is_found_joined_and_its_channel_read() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let mut bob = Peer::enroll(&stack, "bob").await;
  let space: SpaceId = stack
    .create_space(&alice.tokens, "Project A", "open")
    .await
    .parse()
    .expect("space ID");
  let channel = alice
    .client
    .create_channel_with(
      space,
      ChannelOptions {
        name: "general".into(),
        is_private: false,
        maybe_topic: None,
        maybe_category: None,
        maybe_history: None,
        maybe_idempotency_key: None,
      },
    )
    .await
    .expect("creating the channel");

  // Carol browses: the open space is listed with its size, and she is not in it yet.
  let spaces = stack.get(&carol.tokens, "/spaces").await.expect(StatusCode::OK);
  let listed = spaces.body["spaces"]
    .as_array()
    .and_then(|list| list.iter().find(|entry| entry["id"] == space.to_string()))
    .cloned()
    .unwrap_or_else(|| panic!("the open space is not listed: {}", spaces.body));
  assert_eq!(listed["member_count"], 1, "{listed}");
  assert!(listed.get("my_role").is_none(), "{listed}");
  carol.client.join_space(space).await.expect("joining the space");

  // Bob is picked from the directory by the space's admin; picking them twice adds them once.
  alice
    .client
    .add_space_members(space, &[bob.user_id(), bob.user_id(), carol.user_id()], false)
    .await
    .expect("adding from the directory");
  let roles = stack
    .get(&alice.tokens, &format!("/spaces/{space}/members"))
    .await
    .expect(StatusCode::OK);
  let members = roles.body["members"].as_array().cloned().unwrap_or_default();
  assert_eq!(members.len(), 3, "{}", roles.body);
  assert!(
    members
      .iter()
      .all(|member| member["user"]["id"] == alice.user_id().to_string() || member["role"] == "member"),
    "nobody's role changed: {}",
    roles.body
  );

  // Being in the space opens no channel by itself: each joins it, a member's client adds them.
  for joiner in [&mut carol, &mut bob] {
    joiner
      .client
      .join_channel(channel)
      .await
      .expect("joining the channel");
  }
  let deadline = Instant::now() + Duration::from_secs(30);
  while !(carol.client.has_group(channel).unwrap() && bob.client.has_group(channel).unwrap()) {
    assert!(
      Instant::now() < deadline,
      "the joiners were never added to the group"
    );
    alice.pump().await;
    alice.client.apply_pending_changes().await.unwrap();
    for joiner in [&mut carol, &mut bob] {
      joiner.pump().await;
      joiner.client.sync_welcomes().await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
  alice
    .client
    .send_text(channel, None, "welcome to Project A")
    .await
    .unwrap();
  carol.wait_for_text(channel, "welcome to Project A").await;
  bob.wait_for_text(channel, "welcome to Project A").await;

  stack.stop().await;
}
