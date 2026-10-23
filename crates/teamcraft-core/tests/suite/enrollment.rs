//! Enrollment survives a network cut at every step (e2ee.md §3.2), through the fake server: the
//! retry (also after a restart) finishes it, no orphan device stays behind, and another device can
//! always recover the account with the passphrase. The first owner's case gets its own test, since
//! their identity becomes the workspace root of trust.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{PASSPHRASE, Peer, TestClient, clock, options, settings};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::{FakeServer, FakeServerApi, NetworkCut};
use teamcraft_core::store::{LocalStore, MemoryStore, Table};
use teamcraft_core::workspace::RosterRole;
use teamcraft_core::{Client, CoreError, PendingEnrollment};

const CUTS: [NetworkCut; 2] = [NetworkCut::BeforeRequest, NetworkCut::ResponseLost];
/// More REST calls than an enrollment makes; the loops stop once one gets through uncut.
const MAX_CALLS: u32 = 60;

#[tokio::test]
async fn a_new_identity_finishes_after_a_cut_at_any_step() {
  for cut in CUTS {
    for calls in 0..MAX_CALLS {
      let server = FakeServer::new();
      let api = server.add_user("Owner");
      let me = api.user_id();
      let store = Arc::new(MemoryStore::new());
      server.cut_network_after(me, calls, cut);
      if enroll(&api, &store).await.is_ok() {
        break;
      }
      assert_recoverable(&server, me);
      server.set_offline(me, false);

      let client = restart(&api, &store).await;
      assert_complete(&server, &api, &client).await;
      let recovered = recover_on_new_device(&api).await;
      assert_eq!(
        recovered.identity_fingerprint(),
        client.identity_fingerprint(),
        "{cut:?} after {calls} calls"
      );
      assert!(calls < MAX_CALLS - 1, "enrollment never got through");
    }
  }
}

#[tokio::test]
async fn recovering_an_identity_finishes_after_a_cut_at_any_step() {
  for cut in CUTS {
    for calls in 0..MAX_CALLS {
      let server = FakeServer::new();
      let first = Peer::enroll(&server, "Owner").await;
      let api = first.api.clone();
      let me = api.user_id();
      let store = Arc::new(MemoryStore::new());
      server.cut_network_after(me, calls, cut);
      if enroll(&api, &store).await.is_ok() {
        break;
      }
      server.set_offline(me, false);

      let laptop = restart(&api, &store).await;
      assert_eq!(laptop.identity_fingerprint(), first.client.identity_fingerprint());
      let mut devices = server.active_devices(me);
      devices.sort();
      let mut expected = vec![first.client.device_id(), laptop.device_id()];
      expected.sort();
      assert_eq!(devices, expected, "{cut:?} after {calls} calls: orphan device");
    }
  }
}

#[tokio::test]
async fn an_interrupted_first_owner_never_leaves_an_unrecoverable_root_of_trust() {
  for cut in CUTS {
    for calls in 0..MAX_CALLS {
      let server = FakeServer::new();
      let api = server.add_user("First owner");
      let me = api.user_id();
      let store = Arc::new(MemoryStore::new());
      server.cut_network_after(me, calls, cut);
      let finished = enroll(&api, &store).await.is_ok();
      // Whatever the server holds after the cut can be recovered with the passphrase.
      assert_recoverable(&server, me);
      server.set_offline(me, false);

      let mut owner = restart(&api, &store).await;
      owner.publish_roster(&[(me, RosterRole::Owner)]).await.unwrap();
      let root = owner.workspace_root_fingerprint().unwrap();
      assert_eq!(root.as_deref(), Some(owner.identity_fingerprint().as_str()));

      // The owner lost the device: the passphrase brings back the root's identity, and with it
      // the right to sign the workspace logs.
      let mut replacement = recover_on_new_device(&api).await;
      replacement.sync_workspace().await.unwrap();
      assert_eq!(replacement.workspace_root_fingerprint().unwrap(), root);
      let policy = replacement.workspace_policy().unwrap();
      replacement.publish_policy(policy).await.unwrap();
      if finished {
        break;
      }
      assert!(calls < MAX_CALLS - 1, "enrollment never got through");
    }
  }
}

#[tokio::test]
async fn an_identity_without_a_key_bag_cannot_become_the_root_of_trust() {
  let server = FakeServer::new();
  let mut owner = Peer::enroll(&server, "Owner").await;
  let me = owner.client.user_id();
  // An older client enrolled: identity key, no key bag.
  server.forget_key_bag(me);
  let result = owner.client.publish_roster(&[(me, RosterRole::Owner)]).await;
  assert!(result.is_err(), "the genesis roster needs a recoverable root");
}

#[tokio::test]
async fn a_lost_key_bag_is_not_replaced_by_one_without_recovery() {
  let server = FakeServer::new();
  let mut owner = Peer::enroll(&server, "Owner").await;
  let me = owner.client.user_id();
  server.forget_key_bag(me);
  // The device has no wraps stored for an account it recovered from nothing: uploading would
  // bind the bag to no recovery factor.
  owner
    .store
    .delete(Table::SyncState, "wrapped_backup_keys")
    .unwrap();
  assert!(matches!(
    owner.client.upload_key_bag().await,
    Err(CoreError::KeyBagMissing)
  ));
  assert!(server.key_bag_of(me).is_none());
}

#[tokio::test]
async fn a_device_id_of_another_device_is_refused() {
  let server = FakeServer::new();
  let phone = Peer::enroll(&server, "Owner").await;
  let me = phone.client.user_id();
  let list_before = server.device_list_of(me).unwrap();
  // The server hands the laptop the phone's ID: signing the device list would drop the phone.
  server.force_next_device_id(phone.client.device_id());
  let error = enroll(&phone.api, &Arc::new(MemoryStore::new())).await.err();
  assert!(matches!(error, Some(CoreError::DeviceIdTaken(_))), "{error:?}");
  assert_eq!(server.device_list_of(me).unwrap(), list_before);
}

/// One enrollment attempt with the passphrase.
async fn enroll(api: &FakeServerApi, store: &Arc<MemoryStore>) -> Result<TestClient, CoreError> {
  PendingEnrollment::new()
    .complete(
      api.clone(),
      store.clone(),
      clock(),
      settings(),
      options(PASSPHRASE),
    )
    .await
}

/// What the app does when it starts again: open the stored account and sync, or enroll again.
async fn restart(api: &FakeServerApi, store: &Arc<MemoryStore>) -> TestClient {
  match Client::open(api.clone(), store.clone(), clock(), settings()).unwrap() {
    Some(mut client) => {
      client.full_sync().await.unwrap();
      client
    },
    None => enroll(api, store).await.unwrap(),
  }
}

/// Another device of the user, enrolled with the passphrase alone.
async fn recover_on_new_device(api: &FakeServerApi) -> TestClient {
  enroll(api, &Arc::new(MemoryStore::new())).await.unwrap()
}

/// The server never holds an identity without the key bag to recover it.
fn assert_recoverable(server: &FakeServer, user: teamcraft_proto::UserId) {
  if server.identity_key_of(user).is_some() {
    assert!(server.key_bag_of(user).is_some(), "identity without key bag");
  }
}

/// One device, with key packages and in the signed device list.
async fn assert_complete(server: &FakeServer, api: &FakeServerApi, client: &TestClient) {
  let me = client.user_id();
  assert_eq!(
    server.active_devices(me),
    vec![client.device_id()],
    "orphan device"
  );
  assert!(server.device_list_of(me).is_some(), "device list not published");
  let status = api.key_package_status(client.device_id()).await.unwrap();
  assert!(status.available > 0 && status.has_last_resort);
}
