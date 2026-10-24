//! Catching up after being away (issue #247): every conversation arrives, the marks the UI shows as
//! "catching up" are gone afterwards, also after a catch-up that failed half way.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common::{Peer, link_device, settle};
use teamcraft_core::api::fake::{FakeServer, NetworkCut};
use teamcraft_core::client::is_catching_up;
use teamcraft_core::store::{LocalStore, MessageContent, MessageFilter};
use teamcraft_proto::ConversationId;

#[tokio::test]
async fn a_new_device_catches_up_every_conversation_and_drops_the_marks() {
  let (_server, mut carol, channels) = busy_workspace().await;
  let mut laptop = link_device(&mut carol).await;
  // Another conversation is in focus than the first one: all of them still arrive.
  laptop.catch_up_focus().set(channels[3]);

  laptop.connect_gateway().await.unwrap();

  for (index, channel) in channels.iter().enumerate() {
    assert_eq!(
      texts(laptop.store(), *channel),
      vec![format!("news {index}")],
      "channel {index}"
    );
    assert!(!is_catching_up(laptop.store(), *channel).unwrap());
  }
}

#[tokio::test]
async fn a_catch_up_cut_short_leaves_no_marks() {
  let (server, mut carol, channels) = busy_workspace().await;
  let mut laptop = link_device(&mut carol).await;
  // Enough calls to get into the per-conversation catch-up, not through it.
  server.cut_network_after(laptop.user_id(), 30, NetworkCut::BeforeRequest);

  assert!(laptop.connect_gateway().await.is_err());

  for channel in &channels {
    assert!(!is_catching_up(laptop.store(), *channel).unwrap());
  }
}

/// Alice's five channels with Carol in them and a message in each.
async fn busy_workspace() -> (FakeServer, Peer, Vec<ConversationId>) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let space = alice.space().await;
  let mut channels = Vec::new();
  for index in 0..5 {
    let channel = alice
      .client
      .create_channel(space, &format!("channel {index}"))
      .await
      .unwrap();
    alice
      .client
      .add_member(channel, carol.client.user_id())
      .await
      .unwrap();
    alice
      .client
      .send_text(channel, None, &format!("news {index}"))
      .await
      .unwrap();
    channels.push(channel);
  }
  settle(&mut [&mut alice, &mut carol]).await;
  (server, carol, channels)
}

fn texts(store: &impl LocalStore, conversation: ConversationId) -> Vec<String> {
  store
    .messages(conversation, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .filter_map(|message| match message.content {
      MessageContent::Text { body, .. } => Some(body),
      _ => None,
    })
    .collect()
}
