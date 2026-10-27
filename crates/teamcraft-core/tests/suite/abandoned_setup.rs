//! Setups nobody finishes (issue #415), through the fake server: the device key pair an enrollment
//! stored before it registered the device is dropped once it expired or the user signed out, the
//! device it registered is revoked (also after a restart in between, and when the server could not
//! be reached at first), nothing is dropped while the enrollment could still finish, and no device
//! that an enrolled device uses is revoked.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use common::{PASSPHRASE, Peer, TestClient, options, settings};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::{FakeServer, FakeServerApi, NetworkCut};
use teamcraft_core::clock::ManualClock;
use teamcraft_core::records::{self, AbandonedDevice, AbandonedDevices};
use teamcraft_core::store::{LocalStore, MemoryStore, Table};
use teamcraft_core::upkeep::Upkeep;
use teamcraft_core::{CoreError, LinkOffer, PendingEnrollment};
use teamcraft_proto::{Base64Bytes, DeviceId};

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

#[tokio::test]
async fn a_failure_that_may_pass_keeps_the_registration_to_revoke() {
  let server = FakeServer::new();
  let api = server.add_user("Owner");
  let me = api.user_id();
  let clock = Arc::new(ManualClock::new(START_MS));
  // The network drops at each step of the revocation in turn, the revocation request itself too.
  let mut is_revoked = false;
  for calls in 0..MAX_CALLS {
    let store = Arc::new(MemoryStore::new());
    interrupt_after_registration(&server, &api, &store, &clock).await;
    clock.advance(expiry_ms());
    server.cut_network_after(me, calls, NetworkCut::BeforeRequest);
    let result = drop_abandoned(&api, &store, &clock).await;
    server.set_offline(me, false);
    assert!(!has_pending_device(&store), "the key pair goes in any case");
    if result.is_ok() {
      assert_eq!(abandoned_count(&store), 0);
      assert!(server.active_devices(me).is_empty());
      is_revoked = true;
      break;
    }
    assert_eq!(
      abandoned_count(&store),
      1,
      "cut after {calls} calls: the entry is kept"
    );
    assert_eq!(server.active_devices(me).len(), 1);
    // The next try (as after a restart) revokes it.
    drop_abandoned(&api, &store, &clock).await.unwrap();
    assert_eq!(abandoned_count(&store), 0);
    assert!(server.active_devices(me).is_empty());
  }
  assert!(is_revoked, "the revocation never got through");
}

#[tokio::test]
async fn an_enrolled_store_never_drops_a_pending_key_pair() {
  let server = FakeServer::new();
  let phone = Peer::enroll(&server, "Owner").await;
  let me = phone.client.user_id();
  // A key pair left next to the account (an enrollment that stopped between its last writes on an
  // older version): it is the device's own.
  let account = records::load_account(&*phone.store).unwrap().unwrap();
  let identity_public_key = *account.identity_public_key().as_bytes();
  let leftover = records::PendingDevice {
    user_id: me,
    identity_public_key,
    device: account.device,
  };
  records::save_pending_device(&*phone.store, &leftover, START_MS).unwrap();
  let clock = Arc::new(ManualClock::new(START_MS + 10 * expiry_ms()));

  assert!(!drop_abandoned(&phone.api, &phone.store, &clock).await.unwrap());

  assert!(has_pending_device(&phone.store));
  assert_eq!(abandoned_count(&phone.store), 0);
  assert_eq!(server.active_devices(me), vec![phone.client.device_id()]);
}

/// Review of #415: a copy of the store taken while a setup was unfinished (a restored backup) must
/// not revoke the device the original finished enrolling with the same key pair.
mod in_use {
  use super::*;

  #[tokio::test]
  async fn a_copy_of_an_unfinished_setup_leaves_the_finished_original_alone() {
    let server = FakeServer::new();
    let phone = Peer::enroll(&server, "Owner").await;
    let me = phone.client.user_id();
    let laptop_store = Arc::new(MemoryStore::new());
    let clock = Arc::new(ManualClock::new(START_MS));
    // The laptop recovers the account; the answer to its registration is lost.
    interrupt_registration_of(&server, &phone.api, &laptop_store, &clock, 2).await;
    let copy = copy_of(&laptop_store);

    // The original finishes with the same registration and lists it in the signed device list.
    let laptop = enroll(&phone.api, &laptop_store, &clock).await.unwrap();
    assert_eq!(server.active_devices(me).len(), 2);

    // The copy opens a day later and gives its setup up.
    clock.advance(expiry_ms());
    assert!(drop_abandoned(&phone.api, &copy, &clock).await.unwrap());

    assert!(!has_pending_device(&copy));
    assert_eq!(abandoned_count(&copy), 0, "given up: in use");
    let mut devices = server.active_devices(me);
    devices.sort();
    let mut expected = vec![phone.client.device_id(), laptop.device_id()];
    expected.sort();
    assert_eq!(devices, expected, "the enrolled laptop is not revoked");
  }

  #[tokio::test]
  async fn without_a_device_list_to_check_nothing_is_revoked() {
    let server = FakeServer::new();
    let phone = Peer::enroll(&server, "Owner").await;
    let me = phone.client.user_id();
    let laptop_store = Arc::new(MemoryStore::new());
    let clock = Arc::new(ManualClock::new(START_MS));
    interrupt_registration_of(&server, &phone.api, &laptop_store, &clock, 2).await;
    // The account's identity certified the registration, and the server shows no list that could
    // say whether a device uses it.
    server.forget_device_list(me);

    clock.advance(expiry_ms());
    assert!(drop_abandoned(&phone.api, &laptop_store, &clock).await.unwrap());

    assert_eq!(abandoned_count(&laptop_store), 0, "given up");
    assert_eq!(server.active_devices(me).len(), 2, "not revoked");
  }

  #[tokio::test]
  async fn the_upkeep_leaves_a_device_the_signed_list_names_alone() {
    let server = FakeServer::new();
    let mut phone = Peer::enroll(&server, "Owner").await;
    let laptop = Peer::enroll_as(phone.api.clone(), "Laptop").await;
    let me = phone.client.user_id();
    let identity = identity_of(&phone);
    let laptop_key = registered_key(&phone, laptop.client.device_id()).await;
    // The phone's store names the laptop's registration as abandoned by a setup of this identity.
    records::save_abandoned_devices(
      &*phone.store,
      &AbandonedDevices {
        devices: vec![AbandonedDevice {
          user_id: me,
          public_key: laptop_key,
          identity_public_key: identity,
        }],
        ..AbandonedDevices::default()
      },
    )
    .unwrap();

    Upkeep::new(Duration::from_secs(60)).tick(&mut phone.client).await;

    assert_eq!(abandoned_count(&phone.store), 0, "given up: in use");
    assert_eq!(server.active_devices(me).len(), 2);
  }
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

    PendingEnrollment::forget_unfinished(&api, &*store, &*clock, PATIENCE)
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
    let signed_out = PendingEnrollment::forget_unfinished(&api, &*store, &*clock, PATIENCE).await;
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

  #[tokio::test(start_paused = true)]
  async fn waits_for_a_slow_server_only_as_long_as_its_patience() {
    let server = FakeServer::new();
    let api = server.add_user("Owner");
    let me = api.user_id();
    let store = Arc::new(MemoryStore::new());
    let clock = Arc::new(ManualClock::new(START_MS));
    interrupt_after_registration(&server, &api, &store, &clock).await;
    server.delay_user_devices(me, Duration::from_secs(60 * 60));

    let started = tokio::time::Instant::now();
    PendingEnrollment::forget_unfinished(&api, &*store, &*clock, PATIENCE)
      .await
      .unwrap();

    assert_eq!(started.elapsed(), PATIENCE);
    assert!(!has_pending_device(&store), "the key pair is gone anyway");
    assert_eq!(abandoned_count(&store), 1, "revoked on the next start");
    assert_eq!(server.active_devices(me).len(), 1);
  }

  #[tokio::test]
  async fn leaves_an_enrolled_device_alone() {
    let server = FakeServer::new();
    let phone = Peer::enroll(&server, "Owner").await;
    let me = phone.client.user_id();
    let clock = Arc::new(ManualClock::new(START_MS));

    PendingEnrollment::forget_unfinished(&phone.api, &*phone.store, &*clock, PATIENCE)
      .await
      .unwrap();

    assert!(records::is_account_stored(&*phone.store).unwrap());
    assert_eq!(server.active_devices(me), vec![phone.client.device_id()]);
  }
}

#[tokio::test]
async fn an_enrolled_device_never_revokes_itself() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Owner").await;
  let me = phone.client.user_id();
  // A leftover naming this device's own key (an enrollment that stopped between its last writes on
  // an older version), certified by another identity, so only the own-device check keeps it.
  let own_key = registered_key(&phone, phone.client.device_id()).await;
  records::save_abandoned_devices(
    &*phone.store,
    &AbandonedDevices {
      devices: vec![AbandonedDevice {
        user_id: me,
        public_key: own_key,
        identity_public_key: [9; 32],
      }],
      ..AbandonedDevices::default()
    },
  )
  .unwrap();

  assert_eq!(phone.client.revoke_abandoned_devices().await.unwrap(), 0);

  assert_eq!(server.active_devices(me), vec![phone.client.device_id()]);
  assert_eq!(abandoned_count(&phone.store), 0);
}

#[tokio::test]
async fn registrations_of_another_account_stay_for_it() {
  let server = FakeServer::new();
  let bob = Peer::enroll(&server, "Bob").await;
  let api = server.add_user("Alice");
  let store = Arc::new(MemoryStore::new());
  let clock = Arc::new(ManualClock::new(START_MS));
  // Left by a setup of Bob's on this device; Alice is signed in now.
  let bob_key = registered_key(&bob, bob.client.device_id()).await;
  let bobs = AbandonedDevice {
    user_id: bob.client.user_id(),
    public_key: bob_key,
    identity_public_key: [9; 32],
  };
  records::save_abandoned_devices(
    &*store,
    &AbandonedDevices {
      devices: vec![bobs.clone()],
      ..AbandonedDevices::default()
    },
  )
  .unwrap();

  drop_abandoned(&api, &store, &clock).await.unwrap();

  assert_eq!(
    records::load_abandoned_devices(&*store).unwrap().unwrap().devices,
    vec![bobs]
  );
  assert_eq!(server.active_devices(bob.client.user_id()).len(), 1);
}

#[tokio::test]
async fn a_refusal_for_good_waits_before_asking_again() {
  let server = FakeServer::new();
  let api = server.add_user("Owner");
  let me = api.user_id();
  let store = Arc::new(MemoryStore::new());
  let clock = Arc::new(ManualClock::new(START_MS));
  interrupt_after_registration(&server, &api, &store, &clock).await;
  clock.advance(expiry_ms());
  server.refuse_user_devices(me, true);

  assert!(drop_abandoned(&api, &store, &clock).await.is_err());
  assert_eq!(abandoned_count(&store), 1);
  // Asked again soon after: nothing goes to the server.
  let calls = server.rest_call_count(me);
  clock.advance(60 * 1000);
  assert!(!drop_abandoned(&api, &store, &clock).await.unwrap());
  assert_eq!(server.rest_call_count(me), calls);

  // Once the wait is over and the server agrees, the device goes.
  server.refuse_user_devices(me, false);
  clock.advance(60 * 60 * 1000);
  drop_abandoned(&api, &store, &clock).await.unwrap();
  assert_eq!(abandoned_count(&store), 0);
  assert!(server.active_devices(me).is_empty());
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
  let devices_before = server.active_devices(api.user_id()).len();
  interrupt_registration_of(server, api, store, clock, devices_before + 1).await;
}

/// Enrolls with `api` until the answer to the registration is lost: the key pair is in `store`,
/// the server lists `devices` devices of the user, and no account is stored.
async fn interrupt_registration_of(
  server: &FakeServer,
  api: &FakeServerApi,
  store: &Arc<MemoryStore>,
  clock: &Arc<ManualClock>,
  devices: usize,
) {
  let me = api.user_id();
  for calls in 0..MAX_CALLS {
    server.cut_network_after(me, calls, NetworkCut::ResponseLost);
    let enrolled = enroll(api, store, clock).await;
    server.set_offline(me, false);
    assert!(enrolled.is_err(), "the cut comes before the enrollment finished");
    if server.active_devices(me).len() == devices {
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

/// The public key the server holds for `device` of `peer`'s user.
async fn registered_key(peer: &Peer, device: DeviceId) -> Base64Bytes {
  peer
    .api
    .list_user_devices(peer.client.user_id())
    .await
    .unwrap()
    .devices
    .into_iter()
    .find(|registered| registered.id == device)
    .unwrap()
    .public_key
}

/// The identity key of `peer`'s account.
fn identity_of(peer: &Peer) -> [u8; 32] {
  *records::load_account(&*peer.store)
    .unwrap()
    .unwrap()
    .identity_public_key()
    .as_bytes()
}

/// What the account part of a backup of `store` holds: everything an unfinished setup wrote.
fn copy_of(store: &Arc<MemoryStore>) -> Arc<MemoryStore> {
  let copy = Arc::new(MemoryStore::new());
  for (key, value) in store.list(Table::Account).unwrap() {
    copy.put(Table::Account, &key, &value).unwrap();
  }
  copy
}

fn has_pending_device(store: &Arc<MemoryStore>) -> bool {
  store.get(Table::Account, "pending_device").unwrap().is_some()
}

fn abandoned_count(store: &Arc<MemoryStore>) -> usize {
  records::load_abandoned_devices(&**store)
    .unwrap()
    .map_or(0, |abandoned| abandoned.devices.len())
}

fn expiry_ms() -> i64 {
  i64::try_from(settings().unfinished_enrollment_expiry.as_millis()).unwrap()
}
