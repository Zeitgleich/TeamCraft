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

  mod when_sealing_fails {
    use teamcraft_proto::ErrorCode;

    use super::*;

    /// A 403 isn't asked again for this version in this epoch; once the group moved on, it is.
    #[tokio::test]
    async fn a_refusal_holds_until_the_group_moves() {
      let mut world = World::new().await;
      world.schedule("Refused", 10 * MINUTE_MS).await;
      world.move_epoch().await;
      world.phone.client.refresh_scheduled().await.unwrap();

      world
        .server
        .fail_scheduled_updates(Some((403, ErrorCode::Forbidden)));
      assert_eq!(world.phone.client.seal_stale_scheduled().await.unwrap(), 0);
      let calls = world.calls();
      assert_eq!(world.phone.client.seal_stale_scheduled().await.unwrap(), 0);
      assert_eq!(world.calls(), calls, "not asked again");

      world.server.fail_scheduled_updates(None);
      world.move_epoch().await;
      world.phone.client.sync_mls(world.channel).await.unwrap();
      assert_eq!(world.phone.client.seal_stale_scheduled().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn an_outage_a_lost_sign_in_or_a_bad_request_marks_nothing() {
      for (status, code) in [
        (500, ErrorCode::Internal),
        (401, ErrorCode::Unauthorized),
        (400, ErrorCode::InvalidRequest),
      ] {
        let mut world = World::new().await;
        world.schedule("Later", 10 * MINUTE_MS).await;
        world.move_epoch().await;
        world.phone.client.refresh_scheduled().await.unwrap();

        world.server.fail_scheduled_updates(Some((status, code)));
        assert!(
          world.phone.client.seal_stale_scheduled().await.is_err(),
          "{status}"
        );
        world.server.fail_scheduled_updates(None);
        assert_eq!(
          world.phone.client.seal_stale_scheduled().await.unwrap(),
          1,
          "{status}"
        );
      }
    }

    /// The group moved between catching up and asking: this round catches up and tries again.
    #[tokio::test]
    async fn an_epoch_conflict_catches_up_and_tries_again() {
      let mut world = World::new().await;
      world.schedule("Moving", 10 * MINUTE_MS).await;
      world.move_epoch().await;
      world.phone.client.refresh_scheduled().await.unwrap();

      world
        .server
        .fail_next_scheduled_update((409, ErrorCode::EpochConflict));
      assert_eq!(world.phone.client.seal_stale_scheduled().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn the_loser_of_a_race_drops_its_work_without_a_mark() {
      let mut world = World::new().await;
      world.schedule("Raced", 10 * MINUTE_MS).await;
      world.move_epoch().await;
      world.phone.client.refresh_scheduled().await.unwrap();

      world
        .server
        .fail_next_scheduled_update((409, ErrorCode::VersionConflict));
      assert_eq!(world.phone.client.seal_stale_scheduled().await.unwrap(), 0);
      // The next round reads what is there and seals if it still has to.
      assert_eq!(world.phone.client.seal_stale_scheduled().await.unwrap(), 1);
    }
  }

  #[tokio::test]
  async fn a_retry_with_the_same_id_holds_one_message() {
    let mut world = World::new().await;
    let id = teamcraft_proto::ClientMessageId::from_bytes([3; 16]);
    let message = OutgoingMessage {
      maybe_client_message_id: Some(id),
      ..world.outgoing("Once")
    };
    let due_at_ms = world.server.server_now_ms() + MINUTE_MS;
    world
      .run(CoreCommand::ScheduleMessage {
        message: message.clone(),
        due_at_ms,
      })
      .await;
    world
      .run(CoreCommand::ScheduleMessage { message, due_at_ms })
      .await;
    assert_eq!(world.backend().scheduled_overview().messages.len(), 1);
  }

  mod a_retry_after_the_user_changed_something {
    use teamcraft_proto::{ClientMessageId, Id, Snowflake, ThreadId};

    use super::*;

    #[tokio::test]
    async fn takes_the_new_time() {
      let mut world = World::new().await;
      let message = world.with_id("Retried");
      let first = world.server.server_now_ms() + MINUTE_MS;
      let second = first + 10 * MINUTE_MS;
      world.schedule_at(message.clone(), first).await;
      world.schedule_at(message, second).await;
      let listed = world.phone.api.list_scheduled_messages().await.unwrap().messages;
      assert_eq!(listed.len(), 1);
      assert_eq!(listed[0].due_at.0, second);
    }

    #[tokio::test]
    async fn takes_the_new_reply_target() {
      let mut world = World::new().await;
      let message = world.with_id("Retried");
      let due_at_ms = world.server.server_now_ms() + MINUTE_MS;
      world.schedule_at(message.clone(), due_at_ms).await;
      world
        .phone
        .client
        .send_text(world.channel, None, "Quoted")
        .await
        .unwrap();
      let quoted = world
        .phone
        .store
        .messages(world.channel, MessageFilter::TopLevel)
        .unwrap()
        .into_iter()
        .find_map(|message| message.maybe_server_id)
        .unwrap();
      let replying = OutgoingMessage {
        maybe_reply_to: Some(quoted),
        ..message
      };
      world.schedule_at(replying, due_at_ms).await;
      let listed = world.phone.api.list_scheduled_messages().await.unwrap().messages;
      assert_eq!(listed.len(), 1);
      assert_eq!(listed[0].version, 2);
    }

    #[tokio::test]
    async fn moves_to_the_new_thread() {
      let mut world = World::new().await;
      let message = world.with_id("Retried");
      let due_at_ms = world.server.server_now_ms() + MINUTE_MS;
      world.schedule_at(message.clone(), due_at_ms).await;
      let thread = ThreadId::from_snowflake(Snowflake::new(78).unwrap());
      let in_thread = OutgoingMessage {
        maybe_thread: Some(thread),
        ..message
      };
      world.schedule_at(in_thread, due_at_ms).await;
      let listed = world.phone.api.list_scheduled_messages().await.unwrap().messages;
      assert_eq!(listed.len(), 1);
      assert_eq!(listed[0].maybe_thread_id, Some(thread));
    }

    #[tokio::test]
    async fn changes_nothing_when_nothing_changed() {
      let mut world = World::new().await;
      let message = world.with_id("Retried");
      let due_at_ms = world.server.server_now_ms() + MINUTE_MS;
      world.schedule_at(message.clone(), due_at_ms).await;
      world.schedule_at(message, due_at_ms).await;
      let listed = world.phone.api.list_scheduled_messages().await.unwrap().messages;
      assert_eq!(listed[0].version, 1);
    }

    impl World {
      fn with_id(&self, body: &str) -> OutgoingMessage {
        OutgoingMessage {
          maybe_client_message_id: Some(ClientMessageId::from_bytes([5; 16])),
          ..self.outgoing(body)
        }
      }

      async fn schedule_at(&mut self, message: OutgoingMessage, due_at_ms: i64) {
        self
          .run(CoreCommand::ScheduleMessage { message, due_at_ms })
          .await;
      }
    }
  }

  mod the_upkeep {
    use teamcraft_proto::ErrorCode;

    use super::*;

    const TICK_MS: i64 = 5_000;
    const INTERVAL_MS: i64 = 60_000;

    /// A round that fails isn't repeated on the next tick, and each failure in a row waits longer.
    #[tokio::test]
    async fn backs_off_when_sealing_keeps_failing() {
      let mut world = World::new().await;
      world.schedule("Failing", 500 * MINUTE_MS).await;
      world.move_epoch().await;
      world.phone.client.refresh_scheduled().await.unwrap();
      world
        .server
        .fail_scheduled_updates(Some((400, ErrorCode::InvalidRequest)));

      let mut upkeep = Upkeep::new(Duration::from_secs(60));
      assert_eq!(world.rounds_within(&mut upkeep, TICK_MS).await, 1, "the first");
      for (window_ms, label) in [
        (INTERVAL_MS, "after one failure"),
        (2 * INTERVAL_MS, "after two"),
        (4 * INTERVAL_MS, "after three"),
        (8 * INTERVAL_MS, "after four"),
        // The cap: not 16 intervals.
        (10 * MINUTE_MS, "at the cap"),
        (10 * MINUTE_MS, "still at the cap"),
      ] {
        assert_eq!(world.rounds_within(&mut upkeep, window_ms).await, 1, "{label}");
      }

      world.server.fail_scheduled_updates(None);
      assert_eq!(world.rounds_within(&mut upkeep, 10 * MINUTE_MS).await, 1);
      assert_eq!(world.first_status(), ScheduledStatus::Waiting);
    }

    use teamcraft_proto::ChannelRole;

    /// Without the group there is nothing to seal with: no asking, on any tick.
    #[tokio::test]
    async fn leaves_alone_what_this_device_has_no_group_for() {
      let mut world = World::new().await;
      world.schedule("Removed before its time", 500 * MINUTE_MS).await;
      let phone = world.phone.client.user_id();
      world
        .server
        .set_channel_role_unsigned(world.channel, world.bob.client.user_id(), ChannelRole::Admin);
      world.bob.client.full_sync().await.unwrap();
      world.bob.client.remove_member(world.channel, phone).await.unwrap();
      let removed = world.phone.client.sync_mls(world.channel).await;
      assert!(
        !world.phone.client.has_group(world.channel).unwrap(),
        "{removed:?}"
      );
      world.phone.client.refresh_scheduled().await.unwrap();

      let mut upkeep = Upkeep::new(Duration::from_secs(60));
      assert_eq!(world.rounds_within(&mut upkeep, 10 * MINUTE_MS).await, 0);
    }

    #[tokio::test]
    async fn looks_at_once_when_the_clock_jumps_back() {
      let mut world = World::new().await;
      world.schedule("Jumping", 500 * MINUTE_MS).await;
      world.move_epoch().await;
      world.phone.client.refresh_scheduled().await.unwrap();
      world
        .server
        .fail_scheduled_updates(Some((400, ErrorCode::InvalidRequest)));
      let mut upkeep = Upkeep::new(Duration::from_secs(60));
      assert_eq!(world.rounds_within(&mut upkeep, TICK_MS).await, 1);

      world.clock.advance(-10 * INTERVAL_MS);
      assert_eq!(world.rounds_within(&mut upkeep, TICK_MS).await, 1);
    }

    /// The runner re-reads the store only when the tick says something changed.
    #[tokio::test]
    async fn says_so_when_the_gateway_changed_the_list() {
      let mut world = World::new().await;
      let mut upkeep = Upkeep::new(Duration::from_secs(60));
      upkeep.tick(&mut world.phone.client).await;
      world.clock.advance(TICK_MS);
      assert!(!upkeep.tick(&mut world.phone.client).await, "nothing happened");

      world.phone.client.note_scheduled_list_stale();
      world.clock.advance(TICK_MS);
      assert!(upkeep.tick(&mut world.phone.client).await);
      world.clock.advance(TICK_MS);
      assert!(!upkeep.tick(&mut world.phone.client).await);
    }

    impl World {
      /// How many rounds of sealing got to the server while `window_ms` passed tick by tick.
      async fn rounds_within(&mut self, upkeep: &mut Upkeep, window_ms: i64) -> u64 {
        let before = self.server.scheduled_update_attempts();
        let mut passed = 0;
        while passed < window_ms {
          upkeep.tick(&mut self.phone.client).await;
          self.clock.advance(TICK_MS);
          passed += TICK_MS;
        }
        self.server.scheduled_update_attempts() - before
      }
    }
  }

  #[tokio::test]
  async fn a_burst_of_events_costs_one_read() {
    let mut world = World::new().await;
    let mut upkeep = Upkeep::new(Duration::from_secs(60));
    upkeep.tick(&mut world.phone.client).await;
    let calls = world.calls();
    for _ in 0..5 {
      world.phone.client.note_scheduled_list_stale();
    }
    upkeep.tick(&mut world.phone.client).await;
    world.phone.client.note_scheduled_list_stale();
    upkeep.tick(&mut world.phone.client).await;
    assert_eq!(world.calls(), calls + 1, "the second waits for the pace");
    world.clock.advance(6_000);
    upkeep.tick(&mut world.phone.client).await;
    assert_eq!(world.calls(), calls + 2);
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

  /// Requests this device made so far.
  fn calls(&self) -> u64 {
    self.server.rest_call_count(self.phone.client.user_id())
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
