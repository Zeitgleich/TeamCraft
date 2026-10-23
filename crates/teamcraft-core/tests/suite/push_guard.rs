//! The device's own push decisions (notifications.md, "Mobile push delivery"): duplicates, the
//! second check of mutes and DND, withdrawals that fetch nothing, and the diagnostics.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::push_guard::{self, Suppression};
use teamcraft_core::push_preview::{PreviewContent, push_preview};
use teamcraft_core::records;
use teamcraft_core::store::{LocalStore as _, Table};
use teamcraft_crypto::push::seal_wakeup;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::Id as _;
use teamcraft_proto::{
  ConversationId, ConversationResponse, DndSchedule, MessageId, NotificationSettings, PresenceState,
  PushKind, PushMode, PushProvider, PushWakeUp, TestPushOutcome, UnixMillis,
};

// 2026-01-05 (Monday) 12:00 UTC.
const MONDAY_NOON_MS: i64 = (1_767_571_200 + 12 * 3600) * 1000;

#[tokio::test]
async fn a_withdrawal_fetches_and_shows_nothing() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let message = alice_says(&mut alice, channel, "read elsewhere").await;
  let dismiss = PushWakeUp {
    kind: PushKind::Dismiss,
    maybe_sender_id: None,
    maybe_badge: Some(3),
    ..wake_up(&alice, channel, message)
  };
  let preview = push_preview(bob.store.as_ref(), Some(&bob.api), &seal(&server, &bob, &dismiss))
    .await
    .unwrap();
  assert_eq!(preview.content, PreviewContent::Unavailable);
  assert_eq!(preview.wake_up.kind, PushKind::Dismiss);
  assert_eq!(preview.wake_up.maybe_badge, Some(3));
  settle(&mut [&mut bob]).await;
}

#[tokio::test]
async fn a_ring_in_a_known_conversation_is_verified_so_it_can_be_answered() {
  let (server, alice, mut bob, channel) = channel_with_two().await;
  let ring = PushWakeUp {
    kind: PushKind::Call,
    maybe_message_id: None,
    maybe_call_id: Some(teamcraft_proto::CallId::from_snowflake(
      teamcraft_proto::Snowflake::new(77).unwrap(),
    )),
    ..wake_up(
      &alice,
      channel,
      MessageId::from_snowflake(teamcraft_proto::Snowflake::new(1).unwrap()),
    )
  };
  let preview = push_preview(bob.store.as_ref(), Some(&bob.api), &seal(&server, &bob, &ring))
    .await
    .unwrap();
  assert!(preview.is_conversation_verified);
  // A ring naming a conversation this device doesn't know stays unverified.
  let stranger = PushWakeUp {
    conversation_id: ConversationId::from_snowflake(teamcraft_proto::Snowflake::new(999_999).unwrap()),
    ..ring
  };
  let preview = push_preview(
    bob.store.as_ref(),
    Some(&bob.api),
    &seal(&server, &bob, &stranger),
  )
  .await
  .unwrap();
  assert!(!preview.is_conversation_verified);
  settle(&mut [&mut bob]).await;
}

#[tokio::test]
async fn a_second_wake_up_for_a_shown_message_is_a_duplicate_unless_urgent() {
  let (_server, mut alice, bob, channel) = channel_with_two().await;
  let message = alice_says(&mut alice, channel, "hello").await;
  let first = wake_up(&alice, channel, message);
  let store = bob.store.as_ref();
  assert!(!push_guard::is_duplicate(store, &first).unwrap());
  assert!(push_guard::is_duplicate(store, &first).unwrap());
  let repeat = PushWakeUp {
    is_urgent: true,
    ..first.clone()
  };
  assert!(
    !push_guard::is_duplicate(store, &repeat).unwrap(),
    "urgent repeats alert again"
  );
  // Withdrawals and rings are never duplicates.
  let dismiss = PushWakeUp {
    kind: PushKind::Dismiss,
    ..first
  };
  assert!(!push_guard::is_duplicate(store, &dismiss).unwrap());
}

#[tokio::test]
async fn a_muted_conversation_stays_silent_on_the_device() {
  let (_server, mut alice, bob, channel) = channel_with_two().await;
  let message = alice_says(&mut alice, channel, "muted").await;
  let pushed = wake_up(&alice, channel, message);
  let store = bob.store.as_ref();
  assert_eq!(
    push_guard::local_suppression(store, &pushed, MONDAY_NOON_MS, 0).unwrap(),
    None
  );
  let key = channel.to_string();
  let mut conversation: ConversationResponse = records::get_json(store, Table::Conversations, &key)
    .unwrap()
    .unwrap();
  conversation
    .maybe_my_membership
    .as_mut()
    .unwrap()
    .maybe_muted_until = Some(UnixMillis(MONDAY_NOON_MS + 1000));
  records::put_json(store, Table::Conversations, &key, &conversation).unwrap();
  assert_eq!(
    push_guard::local_suppression(store, &pushed, MONDAY_NOON_MS, 0).unwrap(),
    Some(Suppression::Muted)
  );
  // The mute ran out.
  assert_eq!(
    push_guard::local_suppression(store, &pushed, MONDAY_NOON_MS + 2000, 0).unwrap(),
    None
  );
}

#[tokio::test]
async fn the_dnd_schedule_from_the_server_is_checked_again_with_its_overrides() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let message = alice_says(&mut alice, channel, "during lunch").await;
  // 11:00-13:00 local on Mondays; urgent messages may break through, also in channels.
  let schedule = DndSchedule {
    windows: vec![teamcraft_proto::DndWindow {
      weekdays: vec![teamcraft_proto::Weekday::Monday],
      start_minute: 11 * 60,
      end_minute: 13 * 60,
    }],
    allows_urgent: true,
    // The test conversation is a channel; urgent ones break through only in DMs by default.
    urgent_scope: teamcraft_proto::UrgentScope::Everywhere,
    ..DndSchedule::default()
  };
  server.set_notification_settings(
    bob.client.user_id(),
    NotificationSettings {
      auto_follow_mentioned_threads: true,
      push_mode: PushMode::AwayOnly,
      send_read_receipts: true,
      maybe_dnd_schedule: Some(schedule),
      maybe_uses_desktop_attention: None,
    },
  );
  bob.client.refresh_notification_settings().await.unwrap();
  let store = bob.store.as_ref();
  let pushed = wake_up(&alice, channel, message);
  assert_eq!(
    push_guard::local_suppression(store, &pushed, MONDAY_NOON_MS, 0).unwrap(),
    Some(Suppression::DoNotDisturb)
  );
  // Three hours east, it is already 15:00 on the device.
  assert_eq!(
    push_guard::local_suppression(store, &pushed, MONDAY_NOON_MS, 180).unwrap(),
    None
  );
  let urgent = PushWakeUp {
    is_urgent: true,
    ..pushed
  };
  assert_eq!(
    push_guard::local_suppression(store, &urgent, MONDAY_NOON_MS, 0).unwrap(),
    None
  );
  settle(&mut [&mut bob]).await;
}

#[tokio::test]
async fn manual_dnd_seen_from_the_gateway_silences_the_device() {
  let (_server, mut alice, bob, channel) = channel_with_two().await;
  let message = alice_says(&mut alice, channel, "manual").await;
  let store = bob.store.as_ref();
  records::put_json(
    store,
    Table::Presence,
    &bob.client.user_id().to_string(),
    &PresenceState::DoNotDisturb,
  )
  .unwrap();
  assert_eq!(
    push_guard::local_suppression(store, &wake_up(&alice, channel, message), MONDAY_NOON_MS, 0).unwrap(),
    Some(Suppression::DoNotDisturb)
  );
}

#[tokio::test]
async fn the_diagnostics_remember_the_last_push_and_its_delay() {
  let (_server, mut alice, bob, channel) = channel_with_two().await;
  let message = alice_says(&mut alice, channel, "timed").await;
  let store = bob.store.as_ref();
  assert_eq!(push_guard::push_diagnostics(store).unwrap().received_count, 0);
  let pushed = PushWakeUp {
    maybe_sent_at: Some(UnixMillis(MONDAY_NOON_MS - 1500)),
    ..wake_up(&alice, channel, message)
  };
  push_guard::record_received(store, &pushed, MONDAY_NOON_MS);
  let test = PushWakeUp {
    kind: PushKind::Test,
    ..pushed
  };
  push_guard::record_received(store, &test, MONDAY_NOON_MS + 10);
  let diagnostics = push_guard::push_diagnostics(store).unwrap();
  assert_eq!(diagnostics.received_count, 2);
  assert_eq!(diagnostics.maybe_last_kind, Some(PushKind::Test));
  assert_eq!(diagnostics.maybe_last_delay_ms, Some(1510));
  assert_eq!(diagnostics.maybe_last_test_at_ms, Some(MONDAY_NOON_MS + 10));
}

#[tokio::test]
async fn a_test_push_is_requested_for_this_device() {
  let (_server, _alice, bob, _channel) = channel_with_two().await;
  let response = push_guard::send_test_push(bob.store.as_ref(), &bob.api)
    .await
    .unwrap();
  assert_eq!(response.outcome, TestPushOutcome::Sent);
  assert_eq!(response.maybe_provider, Some(PushProvider::WebPush));
}

async fn channel_with_two() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "planning")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  bob
    .client
    .register_push(PushProvider::WebPush, "{}")
    .await
    .unwrap();
  (server, alice, bob, channel)
}

async fn alice_says(alice: &mut Peer, channel: ConversationId, text: &str) -> MessageId {
  alice.client.send_text(channel, None, text).await.unwrap();
  alice
    .store
    .messages(channel, teamcraft_core::store::MessageFilter::TopLevel)
    .unwrap()
    .last()
    .and_then(|message| message.maybe_server_id)
    .unwrap()
}

fn wake_up(alice: &Peer, channel: ConversationId, message: MessageId) -> PushWakeUp {
  PushWakeUp::for_message(PushKind::Mention, channel, message, alice.client.user_id(), false)
}

fn seal(server: &FakeServer, bob: &Peer, wake_up: &PushWakeUp) -> Vec<u8> {
  let registration = server.push_registration(bob.client.device_id()).unwrap();
  let key = SymmetricKey::from_slice(registration.push_key.as_slice()).unwrap();
  seal_wakeup(&key, &serde_json::to_vec(wake_up).unwrap(), &mut rand_core::OsRng).unwrap()
}
