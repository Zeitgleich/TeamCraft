//! A member the server put into a channel (an instance default) claims the place with a signed
//! self-add, which only ever asks for history from the join on.
// Runs on tokio; the wasm tests are web.rs and web_service_worker.rs.
#![cfg(not(target_arch = "wasm32"))]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Peer, settle};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::workspace_admin::ChannelOptions;
use teamcraft_proto::HistoryPolicy;

#[tokio::test]
async fn a_default_member_joins_the_group_and_reads_only_from_the_join_on() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel_with(
      alice.space().await,
      ChannelOptions {
        maybe_idempotency_key: None,
        name: "general".into(),
        is_private: false,
        maybe_topic: None,
        maybe_category: None,
        // Even a channel that shows its history to members does not show it to a self-add.
        maybe_history: Some(HistoryPolicy::Full),
      },
    )
    .await
    .unwrap();
  alice
    .client
    .send_text(channel, None, "before the join")
    .await
    .unwrap();

  // What the server's default provisioning does: a membership row, no encryption group.
  bob.api.join_conversation(channel).await.unwrap();
  bob.client.full_sync().await.unwrap();
  for _ in 0..10 {
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.apply_pending_changes().await.unwrap();
    bob.client.full_sync().await.unwrap();
  }

  assert!(bob.client.has_group(channel).unwrap(), "Bob was never added");
  alice
    .client
    .send_text(channel, None, "after the join")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  bob.client.load_older_messages(channel).await.unwrap();
  assert!(bob.texts(channel).contains(&"after the join".to_owned()));
  assert!(
    !bob.texts(channel).contains(&"before the join".to_owned()),
    "a default member read the past although nobody chose to show it"
  );
}
