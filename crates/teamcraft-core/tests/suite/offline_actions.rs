//! Actions taken offline (read positions, acknowledgements, pins) and a full sync that keeps going
//! past one failing conversation (issue #159), against the fake server.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, clock, settings, settle};
use teamcraft_core::api::fake::{FakeServer, NetworkCut};
use teamcraft_core::client::PendingAction;
use teamcraft_core::store::{LocalStore, MessageFilter};
use teamcraft_core::{Client, GatewayOutcome, read_state, upkeep};
use teamcraft_proto::{ConversationId, Id, MessageId, Snowflake};

mod queue {
  use super::*;

  #[tokio::test]
  async fn a_read_marker_set_offline_survives_a_restart_and_is_applied_once() {
    let (server, _alice, mut bob, channel) = channel_with_messages(2).await;
    let latest = latest_server_id(&bob, channel);
    let me = bob.client.user_id();

    server.set_offline(me, true);
    bob
      .client
      .queue_action(read(channel, latest))
      .await
      .expect("offline, the action is stored, not failed");
    assert_eq!(bob.client.pending_action_count().unwrap(), 1);
    assert_eq!(server.read_position(channel, me), None);

    // The app is killed and started again: the queue is in the store.
    drop(bob.client);
    let mut restarted = Client::open(bob.api.clone(), bob.store.clone(), clock(), settings())
      .unwrap()
      .unwrap();
    assert_eq!(restarted.pending_action_count().unwrap(), 1);

    server.set_offline(me, false);
    upkeep::flush(&mut restarted).await;
    assert_eq!(server.read_position(channel, me), Some(latest));
    assert_eq!(restarted.pending_action_count().unwrap(), 0);
    // Later flushes find nothing to send: it was applied exactly once.
    upkeep::flush(&mut restarted).await;
    assert_eq!(server.read_position_writes(), 1);
  }

  #[tokio::test]
  async fn a_late_read_marker_never_moves_the_position_back() {
    let (server, _alice, mut bob, channel) = channel_with_messages(3).await;
    let ids = server_ids(&bob, channel);
    let (older, newest) = (ids[0], ids[2]);
    let me = bob.client.user_id();

    // Offline, the newer marker is queued first and an older one (a stale view) after it.
    server.set_offline(me, true);
    bob.client.queue_action(read(channel, newest)).await.unwrap();
    bob.client.queue_action(read(channel, older)).await.unwrap();
    assert_eq!(bob.client.pending_action_count().unwrap(), 1, "coalesced");
    server.set_offline(me, false);
    assert_eq!(bob.client.flush_pending_actions().await.unwrap(), 1);
    assert_eq!(server.read_position(channel, me), Some(newest));

    // Another device's late write of an older position doesn't move it back either.
    bob.client.queue_action(read(channel, older)).await.unwrap();
    assert_eq!(server.read_position(channel, me), Some(newest));
  }

  #[tokio::test]
  async fn an_acknowledgement_whose_answer_was_lost_is_retried_until_it_is_confirmed() {
    let (server, mut alice, mut bob, channel) = channel_with_messages(1).await;
    let message = latest_server_id(&bob, channel);
    let me = bob.client.user_id();

    // The server takes the acknowledgement but the answer never arrives.
    server.cut_network_after(me, 0, NetworkCut::ResponseLost);
    bob
      .client
      .queue_action(PendingAction::Acknowledge {
        conversation: channel,
        message,
      })
      .await
      .unwrap();
    assert_eq!(bob.client.pending_action_count().unwrap(), 1);

    server.set_offline(me, false);
    assert_eq!(bob.client.flush_pending_actions().await.unwrap(), 1);
    assert_eq!(bob.client.pending_action_count().unwrap(), 0);
    // The sender sees bob once, though the server took the acknowledgement twice.
    settle(&mut [&mut alice, &mut bob]).await;
    assert_eq!(
      read_state::acknowledgements(&alice.store, channel, message).unwrap(),
      vec![me]
    );
  }

  #[tokio::test]
  async fn actions_the_server_refuses_for_good_are_dropped_and_do_not_block_the_rest() {
    let (server, _alice, mut bob, channel) = channel_with_messages(1).await;
    let message = latest_server_id(&bob, channel);
    let me = bob.client.user_id();

    server.set_offline(me, true);
    // A conversation that is gone by the time the device is back.
    let gone = ConversationId::from_snowflake(Snowflake::new(4242).unwrap());
    bob.client.queue_action(read(gone, message)).await.unwrap();
    bob.client.queue_action(read(channel, message)).await.unwrap();
    server.set_offline(me, false);

    bob.client.flush_pending_actions().await.unwrap();
    assert_eq!(bob.client.pending_action_count().unwrap(), 0);
    assert_eq!(server.read_position(channel, me), Some(message));
  }

  fn read(conversation: ConversationId, message: MessageId) -> PendingAction {
    PendingAction::ReadPosition {
      conversation,
      message,
    }
  }
}

mod full_sync {
  use super::*;

  #[tokio::test]
  async fn one_forbidden_conversation_does_not_block_the_sync_of_the_others() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let space = alice.space().await;
    let blocked = alice.client.create_channel(space, "blocked").await.unwrap();
    let open = alice.client.create_channel(space, "open").await.unwrap();
    for channel in [blocked, open] {
      alice
        .client
        .add_member(channel, bob.client.user_id())
        .await
        .unwrap();
    }
    settle(&mut [&mut alice, &mut bob]).await;

    // A server restart; meanwhile both channels get a message, and one channel's history answers
    // 403 (as right after a removal the list doesn't show yet).
    server.expire_sessions();
    server.drop_gateways(bob.client.user_id());
    for channel in [blocked, open] {
      alice
        .client
        .send_text(channel, None, "while you were away")
        .await
        .unwrap();
    }
    server.forbid_history(blocked, true);

    let (gateway, outcome) = bob.client.connect_gateway().await.unwrap();
    bob.gateway = gateway;
    assert_eq!(outcome, GatewayOutcome::Fresh);
    assert_eq!(bob.texts(open), vec!["while you were away"]);
    assert_eq!(
      bob.client.conversations_to_resync().unwrap(),
      [blocked].into_iter().collect()
    );

    // Once the conversation answers again, the upkeep catches it up.
    server.forbid_history(blocked, false);
    assert_eq!(bob.client.resync_marked_conversations().await.unwrap(), 1);
    assert_eq!(bob.texts(blocked), vec!["while you were away"]);
    assert!(bob.client.conversations_to_resync().unwrap().is_empty());
  }

  #[tokio::test]
  async fn being_offline_still_fails_the_sync_as_a_whole() {
    let server = FakeServer::new();
    let mut bob = Peer::enroll(&server, "Bob").await;
    server.expire_sessions();
    server.drop_gateways(bob.client.user_id());
    server.set_rest_failing(bob.client.user_id(), true);
    assert!(bob.client.connect_gateway().await.is_err());
    assert!(bob.client.conversations_to_resync().unwrap().is_empty());
  }
}

/// alice and bob in a channel with `count` messages from alice that bob has received.
async fn channel_with_messages(count: usize) -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "general")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  for n in 0..count {
    alice
      .client
      .send_text(channel, None, &format!("message {n}"))
      .await
      .unwrap();
  }
  settle(&mut [&mut alice, &mut bob]).await;
  (server, alice, bob, channel)
}

fn server_ids(peer: &Peer, conversation: ConversationId) -> Vec<MessageId> {
  let mut ids: Vec<MessageId> = peer
    .store
    .messages(conversation, MessageFilter::All)
    .unwrap()
    .into_iter()
    .filter_map(|message| message.maybe_server_id)
    .collect();
  ids.sort();
  ids
}

fn latest_server_id(peer: &Peer, conversation: ConversationId) -> MessageId {
  *server_ids(peer, conversation).last().unwrap()
}
