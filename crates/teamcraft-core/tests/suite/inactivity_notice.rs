//! The inactivity notice of a channel and "keep open" (issue #212): the owners' status arrives with
//! the conversation, keeping the channel open asks the server and reads the channel again, and a
//! failed request leaves the notice as it was. Through the fake server.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::{LocalStore, Table};
use teamcraft_proto::{ChannelInactivityStatus, ConversationId, ConversationResponse, UnixMillis};

#[tokio::test]
async fn keeping_a_quiet_channel_open_replaces_the_notice_with_the_snooze() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let channel = quiet_channel(&mut alice, &server).await;
  assert!(
    stored_status(&alice, channel)
      .unwrap()
      .maybe_notified_at
      .is_some(),
    "the notice arrives with the conversation"
  );

  alice.client.snooze_channel_inactivity(channel).await.unwrap();

  let status = stored_status(&alice, channel).unwrap();
  assert_eq!(status.maybe_notified_at, None, "a snooze forgets the notice");
  assert!(
    status
      .maybe_snoozed_until
      .is_some_and(|until| until.0 > server.server_now_ms())
  );
}

#[tokio::test]
async fn a_channel_nobody_noted_has_no_notice() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let space = alice.space().await;
  let channel = alice.client.create_channel(space, "general").await.unwrap();
  settle(&mut [&mut alice]).await;
  assert_eq!(stored_status(&alice, channel), None);
}

async fn quiet_channel(alice: &mut Peer, server: &FakeServer) -> ConversationId {
  let space = alice.space().await;
  let channel = alice.client.create_channel(space, "general").await.unwrap();
  settle(&mut [alice]).await;
  server.set_inactivity(
    channel,
    Some(ChannelInactivityStatus {
      maybe_notified_at: Some(UnixMillis(1_000)),
      maybe_archive_at: Some(UnixMillis(9_000_000_000_000)),
      ..ChannelInactivityStatus::default()
    }),
  );
  // The server's note reaches the client with the gateway's update event.
  settle(&mut [alice]).await;
  channel
}

fn stored_status(peer: &Peer, channel: ConversationId) -> Option<ChannelInactivityStatus> {
  let bytes = peer
    .store
    .get(Table::Conversations, &channel.to_string())
    .unwrap()?;
  serde_json::from_slice::<ConversationResponse>(&bytes)
    .unwrap()
    .maybe_inactivity
}
