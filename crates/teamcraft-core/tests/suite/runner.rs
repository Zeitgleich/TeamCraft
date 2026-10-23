//! The app-facing runner: connects, flushes the outbox and hands the client back on stop.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use common::{PASSPHRASE, TokioClock, clock, options, settings};
use teamcraft_core::PendingEnrollment;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::runner::{RunnerCommand, RunnerSettings, SyncRunner};
use teamcraft_core::store::{LocalStore, MemoryStore, MessageFilter};

#[tokio::test]
async fn runner_connects_flushes_and_returns_the_client_on_stop() {
  let server = FakeServer::new();
  let api = server.add_user("Alice");
  let mut client = PendingEnrollment::new()
    .complete(
      api.clone(),
      Arc::new(MemoryStore::new()),
      clock(),
      settings(),
      options(PASSPHRASE),
    )
    .await
    .unwrap();
  let space = api.list_spaces().await.unwrap().spaces[0].id;
  let channel = client.create_channel(space, "general").await.unwrap();
  let me = client.user_id();
  server.set_offline(me, true);
  // Offline: the message stays in the outbox (the send itself may report the network error).
  let _ = client.send_text(channel, None, "queued while offline").await;
  let queued = client.store().messages(channel, MessageFilter::TopLevel).unwrap();
  assert!(queued[0].maybe_server_id.is_none());
  server.set_offline(me, false);

  let changes = Rc::new(Cell::new(0));
  let seen = changes.clone();
  let (runner, handle) = SyncRunner::new(
    client,
    RunnerSettings::default(),
    Box::new(move || seen.set(seen.get() + 1)),
  );
  // Commands queued up front are handled in order after connecting, so no timing is involved.
  assert!(handle.send(RunnerCommand::Flush));
  assert!(handle.send(RunnerCommand::Stop));
  let client = runner.run().await;

  assert!(changes.get() >= 2, "connect and flush report changes");
  let messages = client.store().messages(channel, MessageFilter::TopLevel).unwrap();
  assert!(messages[0].maybe_server_id.is_some(), "the outbox was flushed");
}

#[tokio::test(start_paused = true)]
async fn a_busy_runner_keeps_sending_heartbeats() {
  let server = FakeServer::new();
  let client = PendingEnrollment::new()
    .complete(
      server.add_user("Carol"),
      Arc::new(MemoryStore::new()),
      Box::new(TokioClock::new().0),
      settings(),
      options(PASSPHRASE),
    )
    .await
    .unwrap();
  let settings = RunnerSettings::default();
  let heartbeat = settings.connection.max_heartbeat;
  let (runner, handle) = SyncRunner::new(client, settings, Box::new(|| {}));
  // Something wakes the runner every second, more often than its tick: a busy conversation.
  let busy_for = heartbeat * 4;
  let driver = async move {
    let started = tokio::time::Instant::now();
    while started.elapsed() < busy_for {
      tokio::time::sleep(std::time::Duration::from_secs(1)).await;
      assert!(handle.send(RunnerCommand::Flush));
    }
    assert!(handle.send(RunnerCommand::Stop));
  };
  tokio::join!(runner.run(), driver);
  assert!(
    server.heartbeat_count() >= 3,
    "heartbeats while busy: {}",
    server.heartbeat_count()
  );
}

#[tokio::test]
async fn dropping_every_handle_stops_the_runner() {
  let server = FakeServer::new();
  let client = PendingEnrollment::new()
    .complete(
      server.add_user("Bob"),
      Arc::new(MemoryStore::new()),
      clock(),
      settings(),
      options(PASSPHRASE),
    )
    .await
    .unwrap();
  let (runner, handle) = SyncRunner::new(client, RunnerSettings::default(), Box::new(|| {}));
  drop(handle);
  let client = runner.run().await;
  assert!(
    client
      .store()
      .list(teamcraft_core::store::Table::Conversations)
      .is_ok()
  );
}
