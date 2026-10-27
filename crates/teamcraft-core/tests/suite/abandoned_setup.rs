//! Setups nobody finishes (issue #415), through the fake server: the device key pair an enrollment
//! stored before it registered the device is dropped once it expired or the user signed out, the
//! device it registered is revoked (also after a restart in between, and when the server could not
//! be reached at first), and nothing is dropped while the enrollment could still finish.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use common::{PASSPHRASE, Peer, TestClient, options, settings};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::{FakeServer, FakeServerApi, NetworkCut};
use teamcraft_core::clock::ManualClock;
use teamcraft_core::records::{self, AbandonedDevices};
use teamcraft_core::store::{LocalStore, MemoryStore, Table};
use teamcraft_core::upkeep::Upkeep;
use teamcraft_core::{CoreError, LinkOffer, PendingEnrollment};

const START_MS: i64 = 1_800_000_000_000;
/// More REST calls than an enrollment makes before it registers its device.
const MAX_CALLS: u32 = 20;
const PATIENCE: Duration = Duration::from_secs(5);

#[tokio::test]
async fn a_setup_given_up_on_is_dropped_after_the_expiry_and_its_device_revoked() {
  let server = FakeServer::new();
  let api = server.add_user("Owner");
  let me = api.user_id();
  let store = Arc::new(MemoryStore::new());
  let clock = Arc::new(ManualClock::new(START_MS));
  interrupt_after_registration(&server, &api, &store, &clock).await;
  assert_eq!(server.active_devices(me).len(), 1);

  // The app starts again later that day (only the store is left from before): the setup may still
  // be finished, so everything stays.
  let expiry = expiry_ms();
  clock.advance(expiry - 1);
  assert!(!drop_abandoned(&api, &store, &clock).await.unwrap());
  assert!(has_pending_device(&store));
  assert_eq!(server.active_devices(me).len(), 1);

  // A day after the key pair was stored nobody has finished it: it goes, and so does its device.
  clock.advance(1);
  assert!(drop_abandoned(&api, &store, &clock).await.unwrap());
  assert!(!has_pending_device(&store));
  assert_eq!(records::load_abandoned_devices(&*store).unwrap(), None);
  assert!(
    server.active_devices(me).is_empty(),
    "the orphan device is revoked"
  );

  // The user sets the device up after all: one device, the new one.
  let client = enroll(&api, &store, &clock).await.unwrap();
  assert_eq!(server.active_devices(me), vec![client.device_id()]);
}

#[tokio::test]
async fn the_key_pair_stays_while_the_keys_of_a_link_wait_to_be_enrolled() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Owner").await;
  let me = phone.client.user_id();
  let store = Arc::new(MemoryStore::new());
  let clock = Arc::new(ManualClock::new(START_MS));
  receive_link(&mut phone, &store).await;
  // Enrolling with the received keys loses the answer to the laptop's registration (the network).
  let mut is_registered = false;
  for calls in 0..MAX_CALLS {
    server.cut_network_after(me, calls, NetworkCut::ResponseLost);
    let resumed = resume_link(&phone.api, &store, &clock).await;
    server.set_offline(me, false);
    assert!(resumed.is_err(), "the cut comes before the enrollment finished");
    if server.active_devices(me).len() == 2 {
      is_registered = true;
      break;
    }
  }
  assert!(is_registered);
  assert!(!records::is_account_stored(&*store).unwrap());
  assert!(records::has_pending_link(&*store).unwrap());

  // Long after the expiry the link can still finish: nothing is dropped, nothing revoked.
  clock.advance(10 * expiry_ms());
  assert!(!drop_abandoned(&phone.api, &store, &clock).await.unwrap());
  assert!(has_pending_device(&store));
  assert_eq!(server.active_devices(me).len(), 2);

  // The next start finishes the enrollment with that very registration: no orphan.
  let laptop = resume_link(&phone.api, &store, &clock).await.unwrap().unwrap();
  let mut devices = server.active_devices(me);
  devices.sort();
  let mut expected = vec![phone.client.device_id(), laptop.device_id()];
  expected.sort();
  assert_eq!(devices, expected);
  assert!(!has_pending_device(&store));
}

#[tokio::test]
async fn offline_the_key_pair_goes_at_once_and_its_device_after_a_restart() {
  let server = FakeServer::new();
  let api = server.add_user("Owner");
  let me = api.user_id();
  let store = Arc::new(MemoryStore::new());
  let clock = Arc::new(ManualClock::new(START_MS));
  interrupt_after_registration(&server, &api, &store, &clock).await;
  clock.advance(expiry_ms());

  server.set_offline(me, true);
  let offline = drop_abandoned(&api, &store, &clock).await;
  assert!(offline.is_err(), "the revocation waits for the network");
  assert!(!has_pending_device(&store), "the key pair is gone at once");
  assert_eq!(abandoned_count(&store), 1);
  assert_eq!(server.active_devices(me).len(), 1);

  // The app stops here and starts again online: the device is revoked.
  server.set_offline(me, false);
  assert!(
    !drop_abandoned(&api, &store, &clock).await.unwrap(),
    "nothing left to drop"
  );
  assert_eq!(abandoned_count(&store), 0);
  assert!(server.active_devices(me).is_empty());
}

mod sign_out {
  use super::*;

  #[tokio::test]
  async fn drops_the_setup_at_once_and_revokes_its_device() {
    let server = FakeServer::new();
    let api = server.add_user("Owner");
    let me = api.user_id();
    let store = Arc::new(MemoryStore::new());
    let clock = Arc::new(ManualClock::new(START_MS));
    interrupt_after_registration(&server, &api, &store, &clock).await;

    PendingEnrollment::forget_unfinished(&api, &*store, PATIENCE)
      .await
      .unwrap();

    assert!(!has_pending_device(&store));
    assert_eq!(abandoned_count(&store), 0);
    assert!(server.active_devices(me).is_empty());
  }

  #[tokio::test]
  async fn offline_leaves_the_device_to_the_upkeep_of_the_next_setup() {
    let server = FakeServer::new();
    let api = server.add_user("Owner");
    let me = api.user_id();
    let store = Arc::new(MemoryStore::new());
    let clock = Arc::new(ManualClock::new(START_MS));
    interrupt_after_registration(&server, &api, &store, &clock).await;

    server.set_offline(me, true);
    let signed_out = PendingEnrollment::forget_unfinished(&api, &*store, PATIENCE).await;
    assert!(matches!(signed_out, Err(CoreError::Api(_))));
    assert!(!has_pending_device(&store), "the key pair is gone anyway");
    assert_eq!(abandoned_count(&store), 1);
    server.set_offline(me, false);

    // Signed in again, the user sets the device up; the upkeep revokes the old registration.
    let mut client = enroll(&api, &store, &clock).await.unwrap();
    assert_eq!(server.active_devices(me).len(), 2);
    let mut upkeep = Upkeep::new(Duration::from_secs(60));
    upkeep.tick(&mut client).await;
    assert_eq!(server.active_devices(me), vec![client.device_id()]);
    assert_eq!(abandoned_count(&store), 0);
  }

  #[tokio::test]
  async fn leaves_an_enrolled_device_alone() {
    let server = FakeServer::new();
    let phone = Peer::enroll(&server, "Owner").await;
    let me = phone.client.user_id();

    PendingEnrollment::forget_unfinished(&phone.api, &*phone.store, PATIENCE)
      .await
      .unwrap();

    assert!(records::is_account_stored(&*phone.store).unwrap());
    assert_eq!(server.active_devices(me), vec![phone.client.device_id()]);
  }
}

#[tokio::test]
async fn an_enrolled_device_never_revokes_itself() {
  let server = FakeServer::new();
  let phone = Peer::enroll(&server, "Owner").await;
  let me = phone.client.user_id();
  // A leftover naming this device's own key (an enrollment that stopped between its last writes on
  // an older version).
  let own_key = phone
    .api
    .list_user_devices(me)
    .await
    .unwrap()
    .devices
    .into_iter()
    .find(|device| device.id == phone.client.device_id())
    .unwrap()
    .public_key;
  records::save_abandoned_devices(
    &*phone.store,
    &AbandonedDevices {
      user_id: me,
      public_keys: vec![own_key],
    },
  )
  .unwrap();

  assert_eq!(phone.client.revoke_abandoned_devices().await.unwrap(), 0);

  assert_eq!(server.active_devices(me), vec![phone.client.device_id()]);
  assert_eq!(abandoned_count(&phone.store), 0);
}

/// Enrolls a new identity until the network cut comes right as the device is registered (the
/// answer is lost): the key pair is stored and the server lists the device, but no account is
/// stored.
async fn interrupt_after_registration(
  server: &FakeServer,
  api: &FakeServerApi,
  store: &Arc<MemoryStore>,
  clock: &Arc<ManualClock>,
) {
  let me = api.user_id();
  for calls in 0..MAX_CALLS {
    server.cut_network_after(me, calls, NetworkCut::ResponseLost);
    let enrolled = enroll(api, store, clock).await;
    server.set_offline(me, false);
    assert!(enrolled.is_err(), "the cut comes before the enrollment finished");
    if !server.active_devices(me).is_empty() {
      assert!(has_pending_device(store));
      assert!(!records::is_account_stored(&**store).unwrap());
      return;
    }
  }
  panic!("the enrollment never registered its device");
}

/// Links a new device of `owner` whose keys land in `store`, and stops before it enrolls.
async fn receive_link(owner: &mut Peer, store: &Arc<MemoryStore>) {
  let offer = LinkOffer::generate();
  let code = offer.to_code();
  let (new_side, owner_side) = tokio::join!(
    teamcraft_core::client::NewDeviceLink::connect(&owner.api, &offer),
    owner.client.start_link_approval(&code)
  );
  let (approved, received) = tokio::join!(
    owner.client.complete_link_approval(owner_side.unwrap()),
    new_side.unwrap().receive_secrets(store, "Laptop")
  );
  approved.unwrap();
  drop(received.unwrap());
}

async fn enroll(
  api: &FakeServerApi,
  store: &Arc<MemoryStore>,
  clock: &Arc<ManualClock>,
) -> Result<TestClient, CoreError> {
  PendingEnrollment::new()
    .complete(
      api.clone(),
      store.clone(),
      Box::new(clock.clone()),
      settings(),
      options(PASSPHRASE),
    )
    .await
}

async fn resume_link(
  api: &FakeServerApi,
  store: &Arc<MemoryStore>,
  clock: &Arc<ManualClock>,
) -> Result<Option<TestClient>, CoreError> {
  PendingEnrollment::resume_link(
    api.clone(),
    store.clone(),
    Box::new(clock.clone()),
    settings(),
    options(PASSPHRASE).argon2,
  )
  .await
}

/// What the app runs when it starts on a device that is not set up.
async fn drop_abandoned(
  api: &FakeServerApi,
  store: &Arc<MemoryStore>,
  clock: &Arc<ManualClock>,
) -> Result<bool, CoreError> {
  PendingEnrollment::drop_abandoned(api, &**store, &**clock, &settings()).await
}

fn has_pending_device(store: &Arc<MemoryStore>) -> bool {
  store.get(Table::Account, "pending_device").unwrap().is_some()
}

fn abandoned_count(store: &Arc<MemoryStore>) -> usize {
  records::load_abandoned_devices(&**store)
    .unwrap()
    .map_or(0, |abandoned| abandoned.public_keys.len())
}

fn expiry_ms() -> i64 {
  i64::try_from(settings().unfinished_enrollment_expiry.as_millis()).unwrap()
}
