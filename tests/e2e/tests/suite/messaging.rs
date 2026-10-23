//! Encrypted chat: spaces, channels, DMs, threads and reactions between real clients, and the
//! server holding ciphertext only.

use std::time::Duration;

use reqwest::StatusCode;
use teamcraft_core::store::{LocalStore, MessageContent, MessageFilter};
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{Id, ThreadId};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn channel_messages_threads_and_reactions_reach_members_encrypted() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let space = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
  let channel = channel_with(&mut alice, space, "general", &mut [&mut carol]).await;

  // Both sides see the channel in the server's list.
  let listed = stack
    .get(&carol.tokens, "/conversations")
    .await
    .expect(StatusCode::OK);
  assert!(
    listed.body.to_string().contains(&channel.to_string()),
    "{}",
    listed.body
  );

  alice
    .client
    .send_text(channel, None, "plain greeting")
    .await
    .unwrap();
  carol.wait_for_text(channel, "plain greeting").await;
  carol
    .client
    .send_text(channel, None, "greeting back")
    .await
    .unwrap();
  alice.wait_for_text(channel, "greeting back").await;

  // A thread: carol answers alice's message; alice sees the reply in the thread, not in the channel.
  let root_client_id = alice
    .client
    .send_text(channel, None, "thread root")
    .await
    .unwrap();
  let root_server_id = alice
    .store
    .message(channel, root_client_id)
    .unwrap()
    .and_then(|message| message.maybe_server_id)
    .expect("the root was sent");
  let thread = ThreadId::from_snowflake(root_server_id.snowflake());
  carol.wait_for_text(channel, "thread root").await;
  carol
    .client
    .send_text(channel, Some(thread), "thread reply")
    .await
    .unwrap();
  alice
    .wait_until("the thread reply arrives", |peer| {
      peer.thread_texts(channel, thread) == ["thread reply"]
    })
    .await;
  assert!(!alice.texts(channel).contains(&"thread reply".to_owned()));

  // A reaction of carol on alice's root reaches alice.
  carol
    .client
    .react(channel, root_client_id, "👍", true)
    .await
    .unwrap();
  alice
    .wait_until("the reaction arrives", |peer| {
      peer
        .store
        .messages(channel, MessageFilter::All)
        .unwrap()
        .iter()
        .any(|message| matches!(&message.content, MessageContent::Reaction { emoji, is_added: true, .. } if emoji == "👍"))
    })
    .await;

  // The server stores ciphertext only.
  let stored = stack
    .get(
      &alice.tokens,
      &format!("/conversations/{channel}/messages?limit=100"),
    )
    .await
    .expect(StatusCode::OK);
  let raw = stored.body.to_string();
  for secret in [
    "plain greeting",
    "greeting back",
    "thread root",
    "thread reply",
    "👍",
  ] {
    assert!(!raw.contains(secret), "{secret:?} is visible to the server");
  }
  assert!(stored.body["messages"].as_array().unwrap().len() >= 5, "{raw}");
  assert!(
    !stack.server_log().contains("plain greeting"),
    "plaintext in the server log"
  );

  stack.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_messages_are_end_to_end_encrypted_between_two_members() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let mut bob = Peer::enroll(&stack, "bob").await;
  // DMs between workspace members need no shared space.
  let dm = alice.client.open_dm(carol.user_id()).await.unwrap();
  carol.wait_joined(dm).await;

  alice.client.send_text(dm, None, "private hello").await.unwrap();
  carol.wait_for_text(dm, "private hello").await;
  carol.client.send_text(dm, None, "private answer").await.unwrap();
  alice.wait_for_text(dm, "private answer").await;

  // A third member can't read the DM, not even by asking the server.
  let intruder = stack
    .get(&bob.tokens, &format!("/conversations/{dm}/messages"))
    .await;
  assert!(
    matches!(intruder.status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN),
    "{intruder:?}"
  );
  teamcraft_e2e::peer::settle(&mut [&mut bob], Duration::from_secs(1)).await;
  assert!(bob.texts(dm).is_empty());

  stack.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn group_direct_messages_reach_every_participant() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let mut bob = Peer::enroll(&stack, "bob").await;
  // A group DM belongs to no space, like a DM.
  let group = alice
    .client
    .open_group_dm(&[carol.user_id(), bob.user_id()])
    .await
    .unwrap();
  carol.wait_joined(group).await;
  bob.wait_joined(group).await;
  assert_eq!(
    carol
      .client
      .open_group_dm(&[alice.user_id(), bob.user_id()])
      .await
      .unwrap(),
    group,
    "the same people find the same group"
  );

  bob.client.send_text(group, None, "hello both").await.unwrap();
  alice.wait_for_text(group, "hello both").await;
  carol.wait_for_text(group, "hello both").await;

  stack.stop().await;
}
