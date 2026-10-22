//! Removing a member rotates the channel's keys: the removed member can't read what is sent after.

use std::time::Duration;

use reqwest::StatusCode;
use teamcraft_e2e::peer::settle;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_member_cannot_read_messages_sent_after_the_removal() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut bob = Peer::enroll(&stack, "bob").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let space = space_with(
    &stack,
    &alice,
    "Project A",
    &[(&bob, "member"), (&carol, "member")],
  )
  .await;
  let channel = channel_with(&mut alice, space, "team", &mut [&mut bob, &mut carol]).await;

  alice
    .client
    .send_text(channel, None, "before removal")
    .await
    .unwrap();
  carol.wait_for_text(channel, "before removal").await;
  bob.wait_for_text(channel, "before removal").await;

  alice
    .client
    .remove_member(channel, carol.user_id())
    .await
    .unwrap();
  // Bob follows the commit that removes Carol (a new epoch with fresh keys).
  settle(&mut [&mut bob, &mut carol], Duration::from_secs(2)).await;
  alice
    .client
    .send_text(channel, None, "after removal")
    .await
    .unwrap();
  bob.wait_for_text(channel, "after removal").await;
  // Carol gets nothing: the server stops delivering, and she has no key for the new epoch.
  settle(&mut [&mut carol], Duration::from_secs(3)).await;
  assert!(!carol.texts(channel).contains(&"after removal".to_owned()));
  assert!(
    carol.texts(channel).contains(&"before removal".to_owned()),
    "history stays readable"
  );
  let history = stack
    .get(&carol.tokens, &format!("/conversations/{channel}/messages"))
    .await;
  assert!(
    matches!(history.status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN),
    "{history:?}"
  );

  // Whatever Carol still tries to write never reaches the others: the server refuses non-members
  // (the client may queue the attempt in its outbox).
  let _ = carol.client.send_text(channel, None, "from the outside").await;
  settle(&mut [&mut alice, &mut bob], Duration::from_secs(1)).await;
  assert!(!alice.texts(channel).contains(&"from the outside".to_owned()));

  stack.stop().await;
}
