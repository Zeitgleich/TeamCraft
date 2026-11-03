//! Coming back costs what changed (issue #714): with the server's heads, a full sync reads only the
//! conversations whose messages, MLS queue or members moved; without them (an older server) it
//! reads everything, as before.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common::{Peer, clock, settings, settle};
use teamcraft_core::Client;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::{LocalStore, Table};
use teamcraft_proto::{ConversationId, ListConversationMembersResponse};

const CHANNELS: usize = 8;

#[tokio::test]
async fn an_unchanged_workspace_costs_no_request_per_conversation() {
  let (server, _alice, mut carol, _channels) = workspace().await;
  carol.client.full_sync().await.unwrap();

  let before = server.rest_call_count(carol.client.user_id());
  carol.client.full_sync().await.unwrap();
  let with_heads = server.rest_call_count(carol.client.user_id()) - before;

  server.stop_serving_sync_heads();
  let before = server.rest_call_count(carol.client.user_id());
  carol.client.full_sync().await.unwrap();
  let without_heads = server.rest_call_count(carol.client.user_id()) - before;

  // Without heads every conversation costs its member list, MLS queue and history.
  assert!(
    without_heads >= with_heads + 3 * CHANNELS as u64,
    "with heads {with_heads}, without {without_heads}"
  );
  // With them a full sync costs the same few requests, whatever the number of conversations.
  let small = {
    let (server, _alice, mut carol, _channels) = workspace_of(2).await;
    carol.client.full_sync().await.unwrap();
    let before = server.rest_call_count(carol.client.user_id());
    carol.client.full_sync().await.unwrap();
    server.rest_call_count(carol.client.user_id()) - before
  };
  assert_eq!(with_heads, small);
}

/// The app opens again on a workspace that didn't change: the sync loads no MLS group, whatever
/// the number of conversations. Loading every group to look at it cost more than the rest of the
/// sync together (300 channels: about 140 ms in a debug build, issue #714).
#[tokio::test]
async fn an_unchanged_workspace_loads_no_group_after_a_restart() {
  for count in [2, CHANNELS] {
    let (server, _alice, mut carol, _channels) = workspace_of(count).await;
    carol.client.full_sync().await.unwrap();
    carol.client = Client::open(carol.api.clone(), carol.store.clone(), clock(), settings())
      .unwrap()
      .unwrap();
    let before = server.rest_call_count(carol.client.user_id());
    carol.client.full_sync().await.unwrap();

    assert_eq!(
      carol.client.group_loads(),
      0,
      "{count} channels: groups loaded by a sync of an unchanged workspace"
    );
    assert!(server.rest_call_count(carol.client.user_id()) > before);
  }
}

#[tokio::test]
async fn what_changed_while_away_arrives() {
  let (server, mut alice, mut carol, channels) = workspace().await;
  let dave = Peer::enroll(&server, "Dave").await;
  carol.client.full_sync().await.unwrap();

  alice
    .client
    .send_text(channels[2], None, "while you were away")
    .await
    .unwrap();
  alice
    .client
    .add_member(channels[5], dave.client.user_id())
    .await
    .unwrap();
  carol.client.full_sync().await.unwrap();

  assert!(
    carol
      .texts(channels[2])
      .contains(&"while you were away".to_string())
  );
  assert!(member_ids(&carol, channels[5]).contains(&dave.client.user_id()));
  // And the next sync, with nothing new, still has them.
  carol.client.full_sync().await.unwrap();
  assert!(member_ids(&carol, channels[5]).contains(&dave.client.user_id()));
}

#[tokio::test]
async fn an_older_server_without_heads_still_syncs_everything() {
  let (server, mut alice, mut carol, channels) = workspace().await;
  server.stop_serving_sync_heads();
  carol.client.full_sync().await.unwrap();
  alice
    .client
    .send_text(channels[0], None, "from an older server")
    .await
    .unwrap();

  carol.client.full_sync().await.unwrap();

  assert!(
    carol
      .texts(channels[0])
      .contains(&"from an older server".to_string())
  );
}

async fn workspace() -> (FakeServer, Peer, Peer, Vec<ConversationId>) {
  workspace_of(CHANNELS).await
}

/// Alice's space with `count` channels Carol is in, one message each.
async fn workspace_of(count: usize) -> (FakeServer, Peer, Peer, Vec<ConversationId>) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let space = alice.space().await;
  let mut channels = Vec::new();
  for index in 0..count {
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
  (server, alice, carol, channels)
}

fn member_ids(peer: &Peer, conversation: ConversationId) -> Vec<teamcraft_proto::UserId> {
  let bytes = peer
    .store
    .get(Table::Members, &conversation.to_string())
    .unwrap()
    .unwrap();
  let list: ListConversationMembersResponse = serde_json::from_slice(&bytes).unwrap();
  list.members.iter().map(|member| member.user.id).collect()
}
