//! Actions taken offline (read positions, acknowledgements, pins) and a full sync that keeps going
//! past one failing conversation (issue #159), against the fake server.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use common::{Peer, clock, settings, settle};
use teamcraft_core::api::fake::{FakeServer, NetworkCut};
use teamcraft_core::client::{PendingAction, PendingActionSettings};
use teamcraft_core::clock::ManualClock;
use teamcraft_core::store::{LocalStore, MessageFilter, Table};
use teamcraft_core::{Client, ClientSettings, GatewayOutcome, read_state, upkeep};
use teamcraft_proto::{ConversationId, Id, MessageId, Snowflake};

const START_MS: i64 = 1_800_000_000_000;

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

  #[tokio::test]
  async fn an_entry_of_a_newer_version_neither_blocks_the_queue_nor_is_touched() {
    let (server, _alice, mut bob, channel) = channel_with_messages(1).await;
    let message = latest_server_id(&bob, channel);
    let me = bob.client.user_id();
    let newer = br#"{"action":{"kind":"set_status","text":"away"},"queued_at_ms":1}"#;
    bob.store.put(Table::PendingActions, "status", newer).unwrap();

    bob.client.queue_action(read(channel, message)).await.unwrap();
    assert_eq!(server.read_position(channel, me), Some(message));
    assert_eq!(
      bob.store.get(Table::PendingActions, "status").unwrap().as_deref(),
      Some(&newer[..]),
      "left for the version that knows it"
    );
  }

  #[tokio::test]
  async fn two_writers_on_one_store_keep_each_other_s_entries() {
    let (server, _alice, bob, channel) = channel_with_messages(2).await;
    let ids = server_ids(&bob, channel);
    let me = bob.client.user_id();
    let mut tab = Client::open(bob.api.clone(), bob.store.clone(), clock(), settings())
      .unwrap()
      .unwrap();
    let mut other_tab = Client::open(bob.api.clone(), bob.store.clone(), clock(), settings())
      .unwrap()
      .unwrap();

    server.set_offline(me, true);
    tab.queue_action(read(channel, ids[1])).await.unwrap();
    other_tab
      .queue_action(PendingAction::Acknowledge {
        conversation: channel,
        message: ids[0],
      })
      .await
      .unwrap();
    // A stale tab's older position doesn't undo the newer one the other tab queued.
    other_tab.queue_action(read(channel, ids[0])).await.unwrap();
    assert_eq!(tab.pending_action_count().unwrap(), 2);
    assert_eq!(other_tab.pending_action_count().unwrap(), 2);

    server.set_offline(me, false);
    assert_eq!(tab.flush_pending_actions().await.unwrap(), 2);
    assert_eq!(other_tab.flush_pending_actions().await.unwrap(), 0);
    assert_eq!(server.read_position(channel, me), Some(ids[1]));
  }

  #[tokio::test]
  async fn an_entry_stuck_on_server_trouble_backs_off_alone_and_gives_up() {
    let (server, _alice, bob, [stuck, open]) = two_channels().await;
    let (stuck_message, open_message) = (latest_server_id(&bob, stuck), latest_server_id(&bob, open));
    let me = bob.client.user_id();
    let clock = Arc::new(ManualClock::new(START_MS));
    let mut bob = reopen(bob, &clock);

    server.fail_read_positions(stuck, true);
    bob.client.queue_action(read(stuck, stuck_message)).await.unwrap();
    // The other entry goes out although the stuck one was queued first.
    bob.client.queue_action(read(open, open_message)).await.unwrap();
    assert_eq!(server.read_position(open, me), Some(open_message));
    assert_eq!(bob.client.pending_action_count().unwrap(), 1);

    // Before its retry time the stuck one isn't tried again.
    let writes = server.read_position_writes();
    upkeep::flush(&mut bob.client).await;
    assert_eq!(server.read_position_writes(), writes);
    // Each try waits twice as long; after the third failure it is dropped.
    clock.advance(1000);
    upkeep::flush(&mut bob.client).await;
    assert_eq!(server.read_position_writes(), writes + 1);
    assert_eq!(bob.client.pending_action_count().unwrap(), 1);
    clock.advance(1000);
    upkeep::flush(&mut bob.client).await;
    assert_eq!(server.read_position_writes(), writes + 1, "still backing off");
    clock.advance(1000);
    upkeep::flush(&mut bob.client).await;
    assert_eq!(server.read_position_writes(), writes + 2);
    assert_eq!(bob.client.pending_action_count().unwrap(), 0);
    assert_eq!(server.read_position(stuck, me), None);
  }

  #[tokio::test]
  async fn an_entry_older_than_the_age_cap_is_dropped_unsent() {
    let (server, _alice, bob, channel) = channel_with_messages(1).await;
    let message = latest_server_id(&bob, channel);
    let me = bob.client.user_id();
    let clock = Arc::new(ManualClock::new(START_MS));
    let mut bob = reopen(bob, &clock);

    server.set_offline(me, true);
    bob.client.queue_action(read(channel, message)).await.unwrap();
    clock.advance(61 * 1000);
    server.set_offline(me, false);
    assert_eq!(bob.client.flush_pending_actions().await.unwrap(), 0);
    assert_eq!(bob.client.pending_action_count().unwrap(), 0);
    assert_eq!(server.read_position(channel, me), None);
  }

  /// The peer's client again, on a clock the test moves and with short queue limits.
  fn reopen(peer: Peer, clock: &Arc<ManualClock>) -> Peer {
    let client_settings = ClientSettings {
      pending_actions: PendingActionSettings {
        retry_after: Duration::from_secs(1),
        retry_max: Duration::from_secs(60),
        max_attempts: 3,
        max_age: Duration::from_secs(60),
        ..PendingActionSettings::default()
      },
      ..settings()
    };
    let client = Client::open(
      peer.api.clone(),
      peer.store.clone(),
      Box::new(clock.clone()),
      client_settings,
    )
    .unwrap()
    .unwrap();
    Peer { client, ..peer }
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
  async fn a_failed_member_refresh_stays_marked_after_the_content_synced() {
    let (server, mut alice, mut bob, [broken, _]) = two_channels().await;
    server.expire_sessions();
    server.drop_gateways(bob.client.user_id());
    alice
      .client
      .send_text(broken, None, "while you were away")
      .await
      .unwrap();
    server.break_member_list(broken, true);

    let (gateway, outcome) = bob.client.connect_gateway().await.unwrap();
    bob.gateway = gateway;
    assert_eq!(outcome, GatewayOutcome::Fresh);
    // The content came through, the member list did not: only that waits for another try.
    assert!(bob.texts(broken).contains(&"while you were away".to_owned()));
    let marked = [broken].into_iter().collect();
    assert_eq!(bob.client.conversations_to_resync_members().unwrap(), marked);
    assert_eq!(bob.client.conversations_to_resync().unwrap(), marked);

    // Still broken: the upkeep's retry leaves it marked.
    assert_eq!(bob.client.resync_marked_conversations().await.unwrap(), 0);
    assert_eq!(bob.client.conversations_to_resync_members().unwrap(), marked);
    server.break_member_list(broken, false);
    assert_eq!(bob.client.resync_marked_conversations().await.unwrap(), 1);
    assert!(bob.client.conversations_to_resync().unwrap().is_empty());
  }

  #[tokio::test]
  async fn after_a_server_restart_the_full_sync_brings_what_was_missed_and_sends_the_queue() {
    let (server, mut alice, mut bob, channel) = channel_with_messages(1).await;
    let message = latest_server_id(&bob, channel);
    let me = bob.client.user_id();
    server.set_offline(me, true);
    bob.client.queue_action(read(channel, message)).await.unwrap();

    // The server restarts: it knows no session any more, so the resume fails.
    server.expire_sessions();
    server.drop_gateways(me);
    alice
      .client
      .send_text(channel, None, "after the restart")
      .await
      .unwrap();
    server.set_offline(me, false);
    let (gateway, outcome) = bob.client.connect_gateway().await.unwrap();
    bob.gateway = gateway;
    assert_eq!(outcome, GatewayOutcome::Fresh, "session_invalid: a full sync");
    assert_eq!(bob.texts(channel).last().unwrap(), "after the restart");
    assert_eq!(server.read_position(channel, me), Some(message));
    assert_eq!(bob.client.pending_action_count().unwrap(), 0);
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

/// alice and bob in two channels with one message from alice in each that bob has received.
async fn two_channels() -> (FakeServer, Peer, Peer, [ConversationId; 2]) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let space = alice.space().await;
  let mut channels = Vec::new();
  for name in ["first", "second"] {
    let channel = alice.client.create_channel(space, name).await.unwrap();
    alice
      .client
      .add_member(channel, bob.client.user_id())
      .await
      .unwrap();
    channels.push(channel);
  }
  settle(&mut [&mut alice, &mut bob]).await;
  for channel in &channels {
    alice.client.send_text(*channel, None, "hello").await.unwrap();
  }
  settle(&mut [&mut alice, &mut bob]).await;
  (server, alice, bob, [channels[0], channels[1]])
}

fn read(conversation: ConversationId, message: MessageId) -> PendingAction {
  PendingAction::ReadPosition {
    conversation,
    message,
  }
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
