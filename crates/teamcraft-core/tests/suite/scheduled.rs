//! Scheduled messages on the client (docs/scheduled-messages.md): sealed here, held by the server,
//! read by the sender's other devices, and sealed again when the group moves on. The server's own
//! rules are tested against the real server.
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
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::{Clock, ManualClock};
use teamcraft_core::store::{LocalStore, MemoryStore, MessageContent, MessageFilter};
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};
use teamcraft_core::upkeep::Upkeep;
use teamcraft_proto::{ConversationId, ScheduledMessageId};

const MINUTE_MS: i64 = 60_000;

#[tokio::test]
async fn the_server_sends_it_with_the_device_gone_and_the_device_reads_it_as_its_own() {
  let mut world = World::new().await;
  world.schedule("Good morning", MINUTE_MS).await;
  assert_eq!(world.first_status(), ScheduledStatus::Waiting);
  assert_eq!(world.overview_body(), "Good morning");

  world.move_time(MINUTE_MS);
  assert_eq!(world.phone.api.release_due_scheduled().await, 1);
  world.phone.client.full_sync().await.unwrap();
  world.phone.client.refresh_scheduled().await.unwrap();
  assert_eq!(world.bodies(), ["Good morning"]);
  assert!(world.backend().scheduled_overview().messages.is_empty());
}

#[tokio::test]
async fn nothing_plain_reaches_the_server() {
  let mut world = World::new().await;
  world.schedule("the secret plan", MINUTE_MS).await;
  let listed = world.phone.api.list_scheduled_messages().await.unwrap();
  let text = serde_json::to_string(&listed).unwrap();
  assert!(!text.contains("the secret plan"));
  use base64::Engine;
  let ciphertext = listed.messages[0].ciphertext.as_slice();
  let as_text = String::from_utf8_lossy(ciphertext);
  assert!(!as_text.contains("the secret plan"));
  assert!(
    !base64::engine::general_purpose::STANDARD
      .encode(ciphertext)
      .is_empty()
  );
}

#[tokio::test]
async fn the_other_device_reads_the_text_and_can_change_it() {
  let mut world = World::new().await;
  let mut laptop = world.linked_laptop().await;
  world.schedule("From the phone", MINUTE_MS).await;
  laptop.refresh_scheduled().await.unwrap();
  let backend = CoreChatBackend::new(
    laptop.store().clone(),
    laptop.user_id(),
    Box::new(Arc::clone(&world.clock)),
  );
  let overview = backend.scheduled_overview();
  assert_eq!(overview.messages[0].body, "From the phone");
  let id = overview.messages[0].id;
  let due_at = overview.messages[0].due_at;

  laptop
    .run_ui_command(CoreCommand::UpdateScheduled {
      id,
      body: "From the laptop".into(),
      due_at_ms: due_at,
    })
    .await
    .unwrap();
  world.phone.client.refresh_scheduled().await.unwrap();
  assert_eq!(world.overview_body(), "From the laptop");
}

#[tokio::test]
async fn cancel_and_send_now_reach_the_server() {
  let mut world = World::new().await;
  world.schedule("Never", MINUTE_MS).await;
  let id = world.first_id();
  world.run(CoreCommand::CancelScheduled { id }).await;
  assert!(world.backend().scheduled_overview().messages.is_empty());
  world.move_time(MINUTE_MS);
  assert_eq!(world.phone.api.release_due_scheduled().await, 0);

  world.schedule("Now", MINUTE_MS).await;
  let id = world.first_id();
  world.run(CoreCommand::SendScheduledNow { id }).await;
  assert_eq!(world.bodies(), ["Now"]);
  assert!(world.backend().scheduled_overview().messages.is_empty());
}

#[tokio::test]
async fn it_keeps_what_the_composer_set() {
  let mut world = World::new().await;
  let message = OutgoingMessage {
    priority: MessagePriority::Important,
    is_silent: true,
    ..world.outgoing("Quietly")
  };
  let due_at_ms = world.clock.now_ms() + MINUTE_MS;
  world
    .run(CoreCommand::ScheduleMessage { message, due_at_ms })
    .await;
  let shown = &world.backend().scheduled_overview().messages[0];
  assert_eq!(shown.priority, MessagePriority::Important);
  assert!(shown.is_silent);
}

mod the_group_moved {
  use super::*;

  #[tokio::test]
  async fn the_upkeep_seals_it_again_ahead_of_its_time() {
    let mut world = World::new().await;
    world.schedule("After the change", 10 * MINUTE_MS).await;
    let before = world.phone.api.list_scheduled_messages().await.unwrap().messages[0].clone();

    world.move_epoch().await;
    world.phone.client.refresh_scheduled().await.unwrap();
    assert_eq!(world.first_status(), ScheduledStatus::NeedsSealing);

    let mut upkeep = Upkeep::new(Duration::from_secs(60));
    upkeep.tick(&mut world.phone.client).await;

    let after = world.phone.api.list_scheduled_messages().await.unwrap().messages[0].clone();
    assert!(after.mls_epoch > before.mls_epoch);
    assert_ne!(after.ciphertext, before.ciphertext);
    assert_eq!(after.due_at, before.due_at, "its time stays");
    assert!(after.is_epoch_current);
    assert_eq!(world.first_status(), ScheduledStatus::Waiting);

    world.move_time(10 * MINUTE_MS);
    assert_eq!(world.phone.api.release_due_scheduled().await, 1);
    world.phone.client.full_sync().await.unwrap();
    assert_eq!(world.bodies(), ["After the change"]);
  }

  #[tokio::test]
  async fn left_stale_at_its_time_it_says_so_and_goes_out_when_a_device_is_back() {
    let mut world = World::new().await;
    world.schedule("Late", MINUTE_MS).await;
    world.move_epoch().await;

    world.move_time(MINUTE_MS);
    assert_eq!(
      world.phone.api.release_due_scheduled().await,
      0,
      "never under the old key"
    );
    world.phone.client.refresh_scheduled().await.unwrap();
    assert_eq!(world.first_status(), ScheduledStatus::Stale);

    world.phone.client.seal_stale_scheduled().await.unwrap();
    assert_eq!(world.first_status(), ScheduledStatus::Waiting);
    assert_eq!(world.phone.api.release_due_scheduled().await, 1);
  }

  #[tokio::test]
  async fn a_wake_up_reads_the_list_and_seals() {
    let mut world = World::new().await;
    world.schedule("Woken", 10 * MINUTE_MS).await;
    world.move_epoch().await;
    world.phone.client.seal_after_wake().await.unwrap();
    assert_eq!(world.first_status(), ScheduledStatus::Waiting);
  }

  /// Two devices seal: the second one's version is old, so it loses, drops its work and doesn't try
  /// that version again.
  #[tokio::test]
  async fn racing_devices_seal_once() {
    let mut world = World::new().await;
    let mut laptop = world.linked_laptop().await;
    world.schedule("Raced", 10 * MINUTE_MS).await;
    laptop.refresh_scheduled().await.unwrap();

    world.move_epoch().await;
    world.phone.client.refresh_scheduled().await.unwrap();
    laptop.refresh_scheduled().await.unwrap();
    assert_eq!(world.phone.client.seal_stale_scheduled().await.unwrap(), 1);

    let version = world.phone.api.list_scheduled_messages().await.unwrap().messages[0].version;
    assert_eq!(laptop.seal_stale_scheduled().await.unwrap(), 0);
    let after = world.phone.api.list_scheduled_messages().await.unwrap().messages[0].clone();
    assert_eq!(after.version, version, "the loser changed nothing");
  }

  #[tokio::test]
  async fn send_now_seals_again_first() {
    let mut world = World::new().await;
    world.schedule("Right away", 10 * MINUTE_MS).await;
    let id = world.first_id();
    world.move_epoch().await;
    world.run(CoreCommand::SendScheduledNow { id }).await;
    assert_eq!(world.bodies(), ["Right away"]);
  }
}

/// A signed-in device with a channel, on a clock that moves together with the server's.
struct World {
  server: FakeServer,
  clock: Arc<ManualClock>,
  phone: Peer,
  bob: Peer,
  channel: ConversationId,
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
    let mut bob = Peer::enroll(&server, "Bob").await;
    let channel = phone
      .client
      .create_channel(phone.space().await, "notes")
      .await
      .unwrap();
    phone
      .client
      .add_member(channel, bob.client.user_id())
      .await
      .unwrap();
    bob.client.sync_welcomes().await.unwrap();
    bob.client.full_sync().await.unwrap();
    phone.client.full_sync().await.unwrap();
    Self {
      server,
      clock,
      phone,
      bob,
      channel,
    }
  }

  async fn linked_laptop(&mut self) -> TestClient {
    let clock = Box::new(Arc::clone(&self.clock));
    let mut laptop = link_device_on_clock(&mut self.phone, settings(), clock).await;
    self.phone.client.add_missing_devices(self.channel).await.unwrap();
    laptop.sync_welcomes().await.unwrap();
    laptop.full_sync().await.unwrap();
    self.phone.client.full_sync().await.unwrap();
    laptop.full_sync().await.unwrap();
    laptop
  }

  /// Real time passing, for this device and the server.
  fn move_time(&self, by_ms: i64) {
    self.clock.advance(by_ms);
    self
      .server
      .advance_clock(Duration::from_millis(u64::try_from(by_ms).unwrap()));
  }

  /// Somebody joins: a commit moves the group to a new epoch.
  async fn move_epoch(&mut self) {
    let joining = Peer::enroll(&self.server, "Joining").await;
    self.bob.client.full_sync().await.unwrap();
    self
      .bob
      .client
      .add_member(self.channel, joining.client.user_id())
      .await
      .unwrap();
  }

  fn outgoing(&self, body: &str) -> OutgoingMessage {
    OutgoingMessage {
      conversation: self.channel,
      maybe_client_message_id: None,
      maybe_thread: None,
      body: body.to_owned(),
      maybe_reply_to: None,
      maybe_reply_attachment: None,
      priority: MessagePriority::Normal,
      wants_acknowledgement: false,
      is_silent: false,
      also_send_to_channel: false,
      files: Vec::new(),
    }
  }

  async fn schedule(&mut self, body: &str, in_ms: i64) {
    let message = self.outgoing(body);
    let due_at_ms = self.server.server_now_ms() + in_ms;
    self
      .run(CoreCommand::ScheduleMessage { message, due_at_ms })
      .await;
  }

  async fn run(&mut self, command: CoreCommand) {
    self.phone.client.run_ui_command(command).await.unwrap();
  }

  fn backend(&self) -> CoreChatBackend<Arc<MemoryStore>> {
    CoreChatBackend::new(
      self.phone.store.clone(),
      self.phone.client.user_id(),
      Box::new(Arc::clone(&self.clock)),
    )
  }

  fn first_id(&self) -> ScheduledMessageId {
    self.backend().scheduled_overview().messages[0].id
  }

  fn first_status(&self) -> ScheduledStatus {
    self.backend().scheduled_overview().messages[0].status
  }

  fn overview_body(&self) -> String {
    self.backend().scheduled_overview().messages[0].body.clone()
  }

  /// The texts the device holds as sent messages of the channel.
  fn bodies(&self) -> Vec<String> {
    self
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
      .collect()
  }
}
