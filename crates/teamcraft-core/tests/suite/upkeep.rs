//! The periodic upkeep asks the server nothing on a timer while all is well (issue #252): account
//! data is read once per gateway connection and when this device changed it.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use crate::common::{PASSPHRASE, TestClient, options, settings};
use teamcraft_core::PendingEnrollment;
use teamcraft_core::api::fake::{FakeGateway, FakeServer};
use teamcraft_core::clock::ManualClock;
use teamcraft_core::store::MemoryStore;
use teamcraft_core::upkeep::Upkeep;

/// The loops' tick (web and desktop: 3–5 s).
const TICK_MS: i64 = 3_000;
const INTERVAL: Duration = Duration::from_secs(60);

#[tokio::test]
async fn an_idle_connected_client_asks_the_server_nothing() {
  let (server, clock, mut client, _gateway) = connected().await;
  let mut upkeep = Upkeep::new(INTERVAL);
  upkeep.tick(&mut client).await;
  let user = client.user_id();
  let before = server.rest_call_count(user);

  // An hour of ticks.
  for _ in 0..1_200 {
    clock.advance(TICK_MS);
    upkeep.tick(&mut client).await;
  }

  assert_eq!(server.rest_call_count(user), before);
}

#[tokio::test]
async fn each_new_connection_reads_the_account_data_once() {
  let (server, clock, mut client, _gateway) = connected().await;
  let mut upkeep = Upkeep::new(INTERVAL);
  upkeep.tick(&mut client).await;
  let user = client.user_id();

  let (_gateway, _) = client.connect_gateway().await.unwrap();
  let before = server.rest_call_count(user);
  for _ in 0..10 {
    clock.advance(TICK_MS);
    upkeep.tick(&mut client).await;
  }

  assert_eq!(
    server.rest_call_count(user),
    before + 1,
    "one key bag read for the new connection"
  );
}

#[tokio::test]
async fn a_failed_sync_is_retried_after_the_interval_not_on_every_tick() {
  let (server, clock, mut client, _gateway) = connected().await;
  let user = client.user_id();
  server.set_offline(user, true);
  let mut upkeep = Upkeep::new(INTERVAL);
  upkeep.tick(&mut client).await;
  let after_first = server.rest_call_count(user);

  for _ in 0..5 {
    clock.advance(TICK_MS);
    upkeep.tick(&mut client).await;
  }
  assert_eq!(
    server.rest_call_count(user),
    after_first,
    "waiting for the interval"
  );

  server.set_offline(user, false);
  clock.advance(60_000);
  upkeep.tick(&mut client).await;
  let after_retry = server.rest_call_count(user);
  assert!(after_retry > after_first, "retried");
  clock.advance(TICK_MS);
  upkeep.tick(&mut client).await;
  assert_eq!(server.rest_call_count(user), after_retry, "and done");
}

async fn connected() -> (FakeServer, Arc<ManualClock>, TestClient, FakeGateway) {
  let server = FakeServer::new();
  let clock = Arc::new(ManualClock::new(1_800_000_000_000));
  let mut client = PendingEnrollment::new()
    .complete(
      server.add_user("Alice"),
      Arc::new(MemoryStore::new()),
      Box::new(clock.clone()),
      settings(),
      options(PASSPHRASE),
    )
    .await
    .unwrap();
  let (gateway, _) = client.connect_gateway().await.unwrap();
  (server, clock, client, gateway)
}
