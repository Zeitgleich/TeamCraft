//! The custom status set in the account menu (PLAN.md §4): it reaches the server through the
//! offline queue, shows at once for the user, and others read it with their presence.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{CustomStatus, StatusExpiry};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::ui_backend::CoreCommand;
use teamcraft_proto::UnixMillis;

/// The clock of the test peers (`common::clock`).
const NOW_MS: i64 = 1_800_000_000_000;
const HOUR_MS: i64 = 3_600_000;

#[tokio::test]
async fn a_status_set_in_the_menu_reaches_the_server_and_others_see_it_with_its_end() {
  let (server, mut alice, mut bob) = two_in_a_channel().await;
  let alice_id = alice.client.user_id();
  let alice_ui = backend_of(&alice);

  alice_ui.set_custom_status(Some(vacation(StatusExpiry::OneHour)));
  run_commands(&mut alice, &alice_ui).await;

  let stored = server.custom_status(alice_id).expect("stored on the server");
  assert_eq!(stored.maybe_emoji.as_deref(), Some("🌴"));
  assert_eq!(stored.maybe_text.as_deref(), Some("Vacation"));
  assert_eq!(stored.maybe_expires_at, Some(UnixMillis(NOW_MS + HOUR_MS)));
  let mine = alice_ui.me().maybe_custom_status.expect("shown to the user");
  assert_eq!(mine.maybe_expires_at, Some(NOW_MS + HOUR_MS));

  // Bob reads it with alice's presence (as after every connection).
  bob.client.refresh_presence(&[alice_id]).await.unwrap();
  let seen = backend_of(&bob)
    .user(alice_id)
    .and_then(|user| user.maybe_custom_status)
    .expect("bob sees alice's status");
  assert_eq!((seen.emoji.as_str(), seen.text.as_str()), ("🌴", "Vacation"));
  assert_eq!(seen.maybe_expires_at, Some(NOW_MS + HOUR_MS));
}

#[tokio::test]
async fn clearing_the_status_removes_it_on_the_server_and_for_the_user() {
  let (server, mut alice, _bob) = two_in_a_channel().await;
  let alice_id = alice.client.user_id();
  let alice_ui = backend_of(&alice);
  alice_ui.set_custom_status(Some(vacation(StatusExpiry::Never)));
  run_commands(&mut alice, &alice_ui).await;
  assert!(server.custom_status(alice_id).is_some());

  alice_ui.set_custom_status(None);
  run_commands(&mut alice, &alice_ui).await;
  assert_eq!(server.custom_status(alice_id), None);
  assert_eq!(alice_ui.me().maybe_custom_status, None);
}

#[tokio::test]
async fn a_status_set_offline_shows_at_once_survives_a_refresh_and_is_sent_once_back() {
  let (server, mut alice, _bob) = two_in_a_channel().await;
  let alice_id = alice.client.user_id();
  let alice_ui = backend_of(&alice);

  server.set_offline(alice_id, true);
  alice_ui.set_custom_status(Some(vacation(StatusExpiry::Never)));
  run_commands(&mut alice, &alice_ui).await;
  assert_eq!(
    alice.client.pending_action_count().unwrap(),
    1,
    "queued, not failed"
  );
  assert!(alice_ui.me().maybe_custom_status.is_some(), "shown while offline");
  assert_eq!(server.custom_status(alice_id), None);

  // Back online, the presence refresh after the reconnect comes before the queue is flushed: the
  // server doesn't know the status yet, but what the user set stays.
  server.set_offline(alice_id, false);
  alice.client.refresh_presence(&[alice_id]).await.unwrap();
  assert!(
    alice_ui.me().maybe_custom_status.is_some(),
    "not undone by the refresh"
  );

  assert_eq!(alice.client.flush_pending_actions().await.unwrap(), 1);
  assert!(server.custom_status(alice_id).is_some());
  assert_eq!(
    alice.client.flush_pending_actions().await.unwrap(),
    0,
    "sent once"
  );
}

#[tokio::test]
async fn a_refused_status_is_reported_and_the_server_s_status_comes_back() {
  let (server, mut alice, _bob) = two_in_a_channel().await;
  let alice_ui = backend_of(&alice);

  // Custom statuses switched off: only presets would be accepted.
  server.refuse_custom_statuses(true);
  let result = alice
    .client
    .run_ui_command(CoreCommand::SetCustomStatus {
      maybe_status: Some(vacation(StatusExpiry::Today)),
    })
    .await;
  assert!(result.is_err(), "the UI hears about the refusal");
  assert_eq!(alice.client.pending_action_count().unwrap(), 0, "dropped");
  assert_eq!(alice_ui.me().maybe_custom_status, None, "back to the server's");
}

#[tokio::test]
async fn a_status_that_ended_while_it_waited_clears_instead() {
  let (server, mut alice, _bob) = two_in_a_channel().await;
  let alice_id = alice.client.user_id();
  alice
    .client
    .set_custom_status(Some(wire("🎧", "Focus", None)))
    .await
    .unwrap();
  assert!(server.custom_status(alice_id).is_some());

  server.set_offline(alice_id, true);
  alice
    .client
    .set_custom_status(Some(wire("🌴", "Out", Some(NOW_MS - 1))))
    .await
    .unwrap();
  server.set_offline(alice_id, false);
  alice.client.flush_pending_actions().await.unwrap();
  assert_eq!(server.custom_status(alice_id), None, "nothing left to show");
}

/// alice and bob share a channel, so they see each other's presence.
async fn two_in_a_channel() -> (FakeServer, Peer, Peer) {
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
  (server, alice, bob)
}

fn vacation(expiry: StatusExpiry) -> CustomStatus {
  CustomStatus {
    emoji: "🌴".into(),
    text: "Vacation".into(),
    expiry,
    maybe_expires_at: None,
  }
}

fn wire(emoji: &str, text: &str, maybe_expires_at: Option<i64>) -> teamcraft_proto::CustomStatus {
  teamcraft_proto::CustomStatus {
    maybe_emoji: Some(emoji.into()),
    maybe_text: Some(text.into()),
    maybe_expires_at: maybe_expires_at.map(UnixMillis),
    maybe_activity: None,
  }
}
