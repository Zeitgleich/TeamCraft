//! Scheduled messages (docs/scheduled-messages.md): held in the encrypted account data, sent once by
//! whichever device of the sender is online at or after the time.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use crate::common;

use common::{Peer, TestClient, link_device_on_clock, settings};
use teamcraft_client_api::actions::{MessagePriority, OutgoingMessage};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::scheduled::ScheduledStatus;
use teamcraft_core::Client;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::{Clock, ManualClock};
use teamcraft_core::pending_work::PendingWork;
use teamcraft_core::store::{LocalStore, MemoryStore, MessageContent, MessageFilter, Priority, Table};
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};
use teamcraft_core::upkeep::Upkeep;
use teamcraft_proto::{ClientMessageId, ConversationId, ScheduledMessagePolicy};

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;

mod one_device {
  use super::*;

  #[tokio::test]
  async fn goes_out_once_at_its_time_and_not_before() {
    let mut world = World::new().await;
    let due = world.server_now() + HOUR_MS;
    world.schedule(1, "Good morning", due).await;

    world.move_time(59 * MINUTE_MS);
    assert_eq!(world.phone.client.send_due_scheduled().await.unwrap(), 0);
    assert!(world.sent_on_server().await.is_empty());

    world.move_time(MINUTE_MS);
    assert_eq!(world.phone.client.send_due_scheduled().await.unwrap(), 1);
    world.phone.client.flush_outbox().await.unwrap();
    assert_eq!(world.sent_on_server().await, ["Good morning"]);

    assert_eq!(world.phone.client.send_due_scheduled().await.unwrap(), 0);
    world.phone.client.flush_outbox().await.unwrap();
    assert_eq!(world.sent_on_server().await, ["Good morning"]);
    assert!(world.phone.client.account_data().scheduled_messages().is_empty());
  }

  #[tokio::test]
  async fn the_upkeep_sends_it() {
    let mut world = World::new().await;
    world.schedule(1, "Tick", world.server_now() + MINUTE_MS).await;
    let mut upkeep = Upkeep::new(Duration::from_secs(60));
    upkeep.tick(&mut world.phone.client).await;
    assert!(world.sent_on_server().await.is_empty());

    world.move_time(MINUTE_MS);
    upkeep.tick(&mut world.phone.client).await;
    assert_eq!(world.sent_on_server().await, ["Tick"]);
  }

  #[tokio::test]
  async fn survives_a_restart() {
    let mut world = World::new().await;
    world
      .schedule(1, "Still here", world.server_now() + MINUTE_MS)
      .await;
    world.phone.client.sync_account_data().await.unwrap();

    world.move_time(MINUTE_MS);
    let mut reopened = Client::open(
      world.phone.api.clone(),
      world.phone.store.clone(),
      Box::new(Arc::clone(&world.clock)),
      settings(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(reopened.account_data().scheduled_messages().len(), 1);
    assert_eq!(reopened.send_due_scheduled().await.unwrap(), 1);
    reopened.flush_outbox().await.unwrap();
    assert_eq!(world.server.stored_ciphertexts().len() - world.baseline, 1);
  }

  #[tokio::test]
  async fn a_crash_between_outbox_and_mark_doesn_t_send_twice() {
    let mut world = World::new().await;
    let id = ClientMessageId::from_bytes([3; 16]);
    world
      .schedule_with_id(id, "Once", world.server_now() + MINUTE_MS)
      .await;
    world.move_time(MINUTE_MS);
    world.phone.client.send_due_scheduled().await.unwrap();
    world.phone.client.flush_outbox().await.unwrap();

    // As after a crash before the mark: the entry is back, and the same ID goes out again.
    world
      .schedule_with_id(id, "Once", world.server_now() - MINUTE_MS)
      .await;
    world.phone.client.send_due_scheduled().await.unwrap();
    world.phone.client.flush_outbox().await.unwrap();
    assert_eq!(world.sent_on_server().await, ["Once"]);
  }

  #[tokio::test]
  async fn can_be_edited_until_its_time() {
    let mut world = World::new().await;
    let id = ClientMessageId::from_bytes([3; 16]);
    let due = world.server_now() + MINUTE_MS;
    world.schedule_with_id(id, "Draft", due).await;
    world
      .run(CoreCommand::UpdateScheduled {
        id,
        body: "Final".into(),
        due_at_ms: due + MINUTE_MS,
      })
      .await;

    world.move_time(MINUTE_MS);
    assert_eq!(world.phone.client.send_due_scheduled().await.unwrap(), 0);
    world.move_time(MINUTE_MS);
    world.phone.client.send_due_scheduled().await.unwrap();
    world.phone.client.flush_outbox().await.unwrap();
    assert_eq!(world.sent_on_server().await, ["Final"]);
  }

  #[tokio::test]
  async fn can_be_cancelled_until_its_time() {
    let mut world = World::new().await;
    let id = ClientMessageId::from_bytes([3; 16]);
    world
      .schedule_with_id(id, "Never", world.server_now() + MINUTE_MS)
      .await;
    world.run(CoreCommand::CancelScheduled { id }).await;

    world.move_time(MINUTE_MS);
    assert_eq!(world.phone.client.send_due_scheduled().await.unwrap(), 0);
    world.phone.client.flush_outbox().await.unwrap();
    assert!(world.sent_on_server().await.is_empty());
  }

  #[tokio::test]
  async fn send_now_doesn_t_wait() {
    let mut world = World::new().await;
    let id = ClientMessageId::from_bytes([3; 16]);
    world
      .schedule_with_id(id, "Now", world.server_now() + HOUR_MS)
      .await;
    world.run(CoreCommand::SendScheduledNow { id }).await;
    assert_eq!(world.sent_on_server().await, ["Now"]);
    assert!(world.phone.client.account_data().scheduled_messages().is_empty());
  }

  #[tokio::test]
  async fn it_keeps_what_the_composer_set() {
    let mut world = World::new().await;
    let message = OutgoingMessage {
      priority: MessagePriority::Important,
      is_silent: true,
      ..world.outgoing("Quietly")
    };
    let due_at_ms = world.server_now() + MINUTE_MS;
    world
      .run(CoreCommand::ScheduleMessage { message, due_at_ms })
      .await;
    world.move_time(MINUTE_MS);
    world.phone.client.send_due_scheduled().await.unwrap();
    let sent = world
      .phone
      .store
      .messages(world.channel, MessageFilter::TopLevel)
      .unwrap();
    let text = serde_json::to_string(&sent.last().unwrap().content).unwrap();
    assert!(text.contains("important"), "{text}");
    assert!(text.contains("is_silent"), "{text}");
  }
}

mod two_devices {
  use super::*;

  #[tokio::test]
  async fn racing_devices_send_it_once() {
    let mut world = World::new().await;
    let mut laptop = world.link_laptop().await;
    let id = ClientMessageId::from_bytes([3; 16]);
    world
      .schedule_with_id(id, "Raced", world.server_now() + MINUTE_MS)
      .await;
    world.phone.client.sync_account_data().await.unwrap();
    world
      .phone
      .client
      .add_missing_devices(world.channel)
      .await
      .unwrap();
    laptop.sync_welcomes().await.unwrap();
    laptop.full_sync().await.unwrap();
    world.phone.client.full_sync().await.unwrap();
    laptop.full_sync().await.unwrap();
    assert_eq!(laptop.account_data().scheduled_messages().len(), 1);

    world.move_time(MINUTE_MS);
    assert_eq!(world.phone.client.send_due_scheduled().await.unwrap(), 1);
    world.phone.client.flush_outbox().await.unwrap();
    assert_eq!(world.sent_on_server().await, ["Raced"]);

    // The laptop still holds the entry, as if it read the key bag a moment before the phone's
    // upload: it sends the same ID, and the server keeps one message.
    laptop
      .run_ui_command(CoreCommand::SendScheduledNow { id })
      .await
      .unwrap();
    let on_laptop = laptop.store().message(world.channel, id).unwrap().unwrap();
    assert!(
      on_laptop.maybe_server_id.is_some(),
      "the laptop's send reached the server"
    );
    assert_eq!(world.sent_on_server().await, ["Raced"]);
    assert!(laptop.account_data().scheduled_messages().is_empty());
  }

  #[tokio::test]
  async fn a_device_coming_back_honours_a_cancel_from_the_other() {
    let mut world = World::new().await;
    let mut laptop = world.link_laptop().await;
    let id = ClientMessageId::from_bytes([3; 16]);
    world
      .schedule_with_id(id, "Cancelled", world.server_now() + MINUTE_MS)
      .await;
    world.phone.client.sync_account_data().await.unwrap();
    world
      .phone
      .client
      .add_missing_devices(world.channel)
      .await
      .unwrap();
    laptop.full_sync().await.unwrap();

    // The phone cancels while the laptop is away; the laptop still holds the entry.
    world.run(CoreCommand::CancelScheduled { id }).await;
    world.phone.client.sync_account_data().await.unwrap();
    assert_eq!(laptop.account_data().scheduled_messages().len(), 1);

    world.move_time(MINUTE_MS);
    assert_eq!(laptop.send_due_scheduled().await.unwrap(), 0);
    assert!(world.sent_on_server().await.is_empty());
    assert!(laptop.account_data().scheduled_messages().is_empty());
  }

  #[tokio::test]
  async fn the_other_device_can_send_what_this_one_wrote() {
    let mut world = World::new().await;
    let mut laptop = world.link_laptop().await;
    world
      .schedule(1, "From the phone", world.server_now() + MINUTE_MS)
      .await;
    world.phone.client.sync_account_data().await.unwrap();
    world
      .phone
      .client
      .add_missing_devices(world.channel)
      .await
      .unwrap();
    laptop.sync_welcomes().await.unwrap();
    laptop.full_sync().await.unwrap();
    world.phone.client.full_sync().await.unwrap();
    laptop.full_sync().await.unwrap();

    world.move_time(MINUTE_MS);
    assert_eq!(laptop.send_due_scheduled().await.unwrap(), 1);
    let report = laptop.flush_outbox().await.unwrap();
    assert_eq!(report.sent, 1, "{report:?}");
    world.phone.client.full_sync().await.unwrap();
    assert_eq!(world.sent_on_server().await, ["From the phone"]);
  }
}

mod clocks {
  use super::*;

  #[tokio::test]
  async fn a_device_clock_ahead_of_the_server_doesn_t_send_early() {
    let mut world = World::new().await;
    world.schedule(1, "Later", world.server_now() + HOUR_MS).await;

    // The device's clock jumps two hours ahead; the server's doesn't.
    world.clock.advance(2 * HOUR_MS);
    assert_eq!(world.phone.client.send_due_scheduled().await.unwrap(), 0);
    assert!(world.sent_on_server().await.is_empty());
    // It looked at the server's clock and now knows better: the next check asks nothing.
    let calls = world.server.rest_call_count(world.phone.client.user_id());
    assert_eq!(world.phone.client.send_due_scheduled().await.unwrap(), 0);
    assert_eq!(world.server.rest_call_count(world.phone.client.user_id()), calls);
  }

  #[tokio::test]
  async fn a_device_clock_behind_the_server_sends_on_the_server_s_time() {
    let mut world = World::new().await;
    world.schedule(1, "On time", world.server_now() + HOUR_MS).await;
    world
      .server
      .advance_clock(Duration::from_millis(u64::try_from(HOUR_MS).unwrap()));
    // Only the server's clock moved; the device learns it from the next key bag.
    world.phone.client.sync_account_data().await.unwrap();

    assert_eq!(world.phone.client.send_due_scheduled().await.unwrap(), 1);
    world.phone.client.flush_outbox().await.unwrap();
    assert_eq!(world.sent_on_server().await, ["On time"]);
  }
}

mod held_back {
  use super::*;

  #[tokio::test]
  async fn one_much_too_late_waits_for_its_owner() {
    let mut world = World::new().await;
    let id = ClientMessageId::from_bytes([3; 16]);
    world
      .schedule_with_id(id, "Stale", world.server_now() + MINUTE_MS)
      .await;

    world.move_time(25 * HOUR_MS);
    assert_eq!(world.phone.client.send_due_scheduled().await.unwrap(), 0);
    assert!(world.sent_on_server().await.is_empty());
    assert_eq!(world.status_of_first(), ScheduledStatus::Missed);

    world.run(CoreCommand::SendScheduledNow { id }).await;
    assert_eq!(world.sent_on_server().await, ["Stale"]);
  }

  #[tokio::test]
  async fn a_policy_that_went_off_stops_it() {
    let mut world = World::new().await;
    world.schedule(1, "Off", world.server_now() + MINUTE_MS).await;
    world.set_policy(ScheduledMessagePolicy {
      is_enabled: false,
      ..ScheduledMessagePolicy::default()
    });

    world.move_time(MINUTE_MS);
    assert_eq!(world.phone.client.send_due_scheduled().await.unwrap(), 0);
    assert!(world.sent_on_server().await.is_empty());
    assert_eq!(world.status_of_first(), ScheduledStatus::PolicyOff);
    assert_eq!(pending(&world).scheduled, 0);
  }

  #[tokio::test]
  async fn offline_it_stays_due_and_goes_out_once_back() {
    let mut world = World::new().await;
    world.schedule(1, "Back", world.server_now() + MINUTE_MS).await;
    world.move_time(MINUTE_MS);
    world.server.set_offline(world.phone.client.user_id(), true);
    assert!(world.phone.client.send_due_scheduled().await.is_err());
    assert_eq!(world.status_of_first(), ScheduledStatus::Due);
    assert_eq!(pending(&world).scheduled, 1);

    world.server.set_offline(world.phone.client.user_id(), false);
    assert_eq!(world.phone.client.send_due_scheduled().await.unwrap(), 1);
    world.phone.client.flush_outbox().await.unwrap();
    assert_eq!(world.sent_on_server().await, ["Back"]);
    assert_eq!(pending(&world).scheduled, 0);
  }

  #[tokio::test]
  async fn a_refused_schedule_changes_nothing() {
    let mut world = World::new().await;
    world.set_policy(ScheduledMessagePolicy {
      is_enabled: false,
      ..ScheduledMessagePolicy::default()
    });
    let message = world.outgoing("Nope");
    let due_at_ms = world.server_now() + MINUTE_MS;
    let outcome = world
      .phone
      .client
      .run_ui_command(CoreCommand::ScheduleMessage { message, due_at_ms })
      .await;
    assert!(outcome.is_err());
    assert!(world.phone.client.account_data().scheduled_messages().is_empty());
  }

  fn pending(world: &World) -> PendingWork {
    PendingWork::read_at(&world.phone.store, world.clock.now_ms()).unwrap()
  }
}

#[tokio::test]
async fn the_list_shows_them_with_their_conversation() {
  let mut world = World::new().await;
  world.schedule(1, "Listed", world.server_now() + MINUTE_MS).await;
  let overview = world.backend().scheduled_overview();
  assert_eq!(overview.messages.len(), 1);
  assert_eq!(overview.messages[0].body, "Listed");
  assert_eq!(overview.messages[0].conversation_name, "notes");
  assert_eq!(overview.messages[0].status, ScheduledStatus::Waiting);
  assert!(overview.policy.is_enabled);
}

/// A signed-in device with a channel, on a clock that moves together with the server's.
struct World {
  server: FakeServer,
  clock: Arc<ManualClock>,
  phone: Peer,
  channel: ConversationId,
  /// Messages on the server before the test's.
  baseline: usize,
}

impl World {
  async fn new() -> Self {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(server.server_now_ms()));
    let mut phone = Peer::enroll_on_clock(
      server.add_user("Alice"),
      "Alice",
      settings(),
      Box::new(Arc::clone(&clock)),
    )
    .await;
    let channel = phone
      .client
      .create_channel(phone.space().await, "notes")
      .await
      .unwrap();
    phone.client.sync_account_data().await.unwrap();
    let baseline = server.stored_ciphertexts().len();
    Self {
      server,
      clock,
      phone,
      channel,
      baseline,
    }
  }

  async fn link_laptop(&mut self) -> TestClient {
    let clock = Box::new(Arc::clone(&self.clock));
    link_device_on_clock(&mut self.phone, settings(), clock).await
  }

  fn server_now(&self) -> i64 {
    self.clock.now_ms()
  }

  /// Moves this device's clock and the server's: real time passing.
  fn move_time(&self, by_ms: i64) {
    self.clock.advance(by_ms);
    self
      .server
      .advance_clock(Duration::from_millis(u64::try_from(by_ms).unwrap()));
  }

  fn outgoing(&self, body: &str) -> OutgoingMessage {
    OutgoingMessage {
      conversation: self.channel,
      maybe_client_message_id: None,
      maybe_thread: None,
      body: body.to_owned(),
      maybe_reply_to: None,
      priority: MessagePriority::Normal,
      wants_acknowledgement: false,
      is_silent: false,
      also_send_to_channel: false,
      files: Vec::new(),
    }
  }

  async fn schedule(&mut self, byte: u8, body: &str, due_at_ms: i64) {
    self
      .schedule_with_id(ClientMessageId::from_bytes([byte; 16]), body, due_at_ms)
      .await;
  }

  async fn schedule_with_id(&mut self, id: ClientMessageId, body: &str, due_at_ms: i64) {
    let message = OutgoingMessage {
      maybe_client_message_id: Some(id),
      ..self.outgoing(body)
    };
    if due_at_ms <= self.server_now() {
      self
        .phone
        .client
        .update_account_data(|data, now| {
          data.set_scheduled(
            teamcraft_core::account_data::ScheduledEntry {
              client_message_id: id,
              conversation_id: message.conversation,
              maybe_thread_id: None,
              body: message.body.clone(),
              due_at_ms,
              maybe_reply_to: None,
              priority: Priority::Normal,
              wants_acknowledgement: false,
              is_silent: false,
              also_send_to_channel: false,
            },
            now.saturating_add(1),
          );
          true
        })
        .unwrap();
    } else {
      self
        .run(CoreCommand::ScheduleMessage { message, due_at_ms })
        .await;
    }
  }

  async fn run(&mut self, command: CoreCommand) {
    self.phone.client.run_ui_command(command).await.unwrap();
  }

  /// The bodies the server stored in the channel, as a reader decrypts them.
  async fn sent_on_server(&mut self) -> Vec<String> {
    let sent: Vec<String> = self
      .phone
      .store
      .messages(self.channel, MessageFilter::TopLevel)
      .unwrap()
      .into_iter()
      .filter(|message| message.maybe_server_id.is_some())
      .filter_map(|message| match message.content {
        MessageContent::Text { body, .. } => Some(body),
        _ => None,
      })
      .collect();
    let stored = self.server.stored_ciphertexts().len() - self.baseline;
    assert_eq!(
      stored,
      sent.len(),
      "the server holds exactly what this device shows as sent"
    );
    sent
  }

  fn backend(&self) -> CoreChatBackend<Arc<MemoryStore>> {
    CoreChatBackend::new(
      self.phone.store.clone(),
      self.phone.client.user_id(),
      Box::new(Arc::clone(&self.clock)),
    )
  }

  fn status_of_first(&self) -> ScheduledStatus {
    self.backend().scheduled_overview().messages[0].status
  }

  fn set_policy(&mut self, scheduled: ScheduledMessagePolicy) {
    let mut state = self.phone.client.workspace().unwrap();
    state.policy.scheduled_messages = scheduled;
    teamcraft_core::records::put_json(&self.phone.store, Table::Workspace, "state", &state).unwrap();
  }
}
