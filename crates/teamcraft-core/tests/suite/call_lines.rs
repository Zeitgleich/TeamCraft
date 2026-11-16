//! Calls in the timeline (docs/notifications.md, "Calls in the timeline"): each device derives a
//! tile per call from the server's call history and the gateway's call events.

use crate::common;

use common::{Peer, backend_of};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{CallTileState, MessageKind};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::{CallHistoryEntry, CallId, ConversationId, UnixMillis, UserId};

const STARTED_MS: i64 = 1_799_000_000_000;

#[tokio::test]
async fn the_call_history_shows_as_tiles_in_the_conversation() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "team")
    .await
    .unwrap();
  let bob_id = bob.client.user_id();
  let call_id = |raw: i64| -> CallId { serde_json::from_value(serde_json::json!(raw.to_string())).unwrap() };
  server.add_call_history(entry(channel, call_id(1), 1, bob_id, Some(185_000)));
  // No end recorded and not running any more: a call the server lost in a restart.
  server.add_call_history(entry(channel, call_id(2), 2, bob_id, None));

  // Two lines written, the lost call's ended in the same read.
  assert_eq!(alice.client.sync_call_lines().await.unwrap(), 3);
  assert_eq!(
    alice.client.sync_call_lines().await.unwrap(),
    0,
    "a second read changes nothing"
  );

  let alice_ui = backend_of(&alice);
  let tiles: Vec<_> = alice_ui
    .feed_tail(channel, None, 50)
    .messages
    .into_iter()
    .filter_map(|message| match message.kind {
      MessageKind::Call(tile) => Some((message.author, tile.state)),
      _ => None,
    })
    .collect();
  assert_eq!(
    tiles,
    [
      (
        bob_id,
        CallTileState::Ended {
          maybe_duration_ms: Some(185_000)
        }
      ),
      (
        bob_id,
        CallTileState::Ended {
          maybe_duration_ms: None
        }
      ),
    ]
  );
  let unread = alice_ui
    .conversation(channel)
    .map(|conversation| conversation.unread_count);
  assert_eq!(unread, Some(0), "a call's tile is nothing to read");
}

fn entry(
  conversation: ConversationId,
  call_id: CallId,
  raw: i64,
  started_by: UserId,
  maybe_duration_ms: Option<i64>,
) -> CallHistoryEntry {
  CallHistoryEntry {
    call_id,
    conversation_id: conversation,
    maybe_started_by: Some(started_by),
    started_at: UnixMillis(STARTED_MS + raw),
    maybe_ended_at: maybe_duration_ms.map(|duration| UnixMillis(STARTED_MS + raw + duration)),
    participants: vec![started_by],
    is_missed: false,
  }
}
