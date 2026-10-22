//! Organization recovery: a 2-of-3 recovery end to end with fake clients that do the real
//! cryptography of teamcraft-crypto, plus the rules the server enforces around it.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::*;
use rand_core::OsRng;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_crypto::backup::{BackupKey, BackupKeyWrapper, WrappedBackupKey};
use teamcraft_crypto::identity::DeviceKeyPair;
use teamcraft_crypto::ids::{DeviceId as CryptoDeviceId, UserId as CryptoUserId};
use teamcraft_crypto::org_recovery::{
  OrgRecoveryPublicKey, OrgRecoverySetup, OrgRecoveryShare, OrgRecoveryWrapper, RecoveryRequestKey,
  RecoveryRequestPublicKey, combine_shares, create_org_recovery,
};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_server::testing::{Reply, TestApp, TestUser};

fn b64(bytes: &[u8]) -> String {
  STANDARD.encode(bytes)
}

fn unb64(value: &Value) -> Vec<u8> {
  STANDARD.decode(value.as_str().unwrap()).unwrap()
}

fn crypto_user(id: &str) -> CryptoUserId {
  CryptoUserId::new(id.parse().unwrap())
}

/// An admin (or any person) with a device whose keys the fake client holds.
struct Person {
  user: TestUser,
  id: String,
  device_id: String,
  keys: DeviceKeyPair,
}

async fn person(app: &TestApp, user: TestUser) -> Person {
  let id = user_id(app, &user).await;
  let device_id = register_device(app, &user).await;
  let keys = DeviceKeyPair::generate(
    crypto_user(&id),
    CryptoDeviceId::new(device_id.parse().unwrap()),
    &mut OsRng,
  );
  Person {
    user,
    id,
    device_id,
    keys,
  }
}

struct Scene {
  app: TestApp,
  /// alice (owner), bob and carl (admins) hold the shares.
  alice: Person,
  bob: Person,
  carl: Person,
  dana: Person,
  setup: OrgRecoverySetup,
  backup_bytes: [u8; 32],
}

/// The org key is set up, the shares are stored and Dana has a key bag with an org-wrapped key.
async fn scene() -> Option<Scene> {
  let app = TestApp::start().await?;
  let alice = person(&app, owner()).await;
  let bob = person(&app, admin()).await;
  let carl = person(&app, TestUser::new("carl", &["admin"])).await;
  let dana = person(&app, member("dana")).await;
  let setup = create_org_recovery(2, 3, &mut OsRng).unwrap();
  let shares: Vec<Value> = [&alice, &bob, &carl]
    .iter()
    .zip(&setup.shares)
    .map(|(admin, share)| {
      let sealed = share
        .seal_for_device(crypto_user(&admin.id), &admin.keys.public_keys().agreement_key)
        .unwrap();
      json!({"device_id": admin.device_id, "sealed": b64(&sealed)})
    })
    .collect();
  let stored = app
    .put(
      &alice.user,
      "/org-recovery/shares",
      json!({"key_version": 1, "shares": shares}),
    )
    .await;
  assert_eq!(stored.status, StatusCode::NO_CONTENT, "{}", stored.body);

  let backup_bytes = [42u8; 32];
  let backup_key = BackupKey::from_key(SymmetricKey::from_bytes(backup_bytes));
  let wrapped = OrgRecoveryWrapper::for_wrapping(setup.public_key)
    .wrap_key(crypto_user(&dana.id), &backup_key, &mut OsRng)
    .unwrap();
  let key_bag = app
    .put(
      &dana.user,
      "/me/key-bag",
      json!({
        "expected_version": 0,
        "encrypted_key_bag": b64(b"key bag ciphertext"),
        "wrapped_backup_keys": b64(&wrapped.to_bytes().unwrap()),
      }),
    )
    .await;
  assert_eq!(key_bag.status, StatusCode::OK, "{}", key_bag.body);
  Some(Scene {
    app,
    alice,
    bob,
    carl,
    dana,
    setup,
    backup_bytes,
  })
}

async fn request_recovery(app: &TestApp, person: &Person) -> (Reply, RecoveryRequestKey) {
  let key = RecoveryRequestKey::generate(&mut OsRng);
  let reply = app
    .post(
      &person.user,
      "/org-recovery/requests",
      json!({"request_public_key": b64(key.public_key().as_bytes())}),
    )
    .await;
  (reply, key)
}

async fn approve(app: &TestApp, admin: &Person, request: &str) -> Reply {
  app
    .post(
      &admin.user,
      &format!("/org-recovery/requests/{request}/approve"),
      json!({"device_id": admin.device_id}),
    )
    .await
}

mod recovery {
  use super::*;

  #[tokio::test]
  async fn two_of_three_admins_recover_a_backup_key_end_to_end() {
    let Some(s) = scene().await else { return };
    let (reply, request_key) = request_recovery(&s.app, &s.dana).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let request = reply.id();
    assert_eq!(reply.body["status"], "pending");
    assert_eq!(reply.body["required_approvals"], 2);
    // The server computed the fingerprint the recovering device shows.
    assert_eq!(reply.body["fingerprint"], request_key.public_key().fingerprint());

    // Admins see it; Bob compares the fingerprint with Dana's screen and approves.
    let listed = s.app.get(&s.bob.user, "/org-recovery/requests").await;
    assert_eq!(listed.body["requests"][0]["id"], request.as_str());
    let first = approve(&s.app, &s.bob, &request).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body["status"], "pending");
    assert!(first.body.get("combining_device_id").is_none());

    // Carl's approval reaches the threshold: his device combines the shares.
    let second = approve(&s.app, &s.carl, &request).await;
    assert_eq!(second.body["status"], "approved");
    assert_eq!(second.body["combining_admin_id"], s.carl.id.as_str());
    assert_eq!(second.body["combining_device_id"], s.carl.device_id.as_str());

    // Bob opens his share and seals it to Carl's device.
    let mine = s.app.get(&s.bob.user, "/org-recovery/shares/mine").await;
    assert_eq!(mine.body["key_version"], 1);
    let bobs_share =
      OrgRecoveryShare::open_on_device(&s.bob.keys, &unb64(&mine.body["shares"][0]["sealed"])).unwrap();
    let for_carl = bobs_share
      .seal_for_device(crypto_user(&s.carl.id), &s.carl.keys.public_keys().agreement_key)
      .unwrap();
    // Carl has not got the other share yet.
    assert_eq!(
      s.app
        .get(
          &s.carl.user,
          &format!("/org-recovery/requests/{request}/material")
        )
        .await
        .status,
      StatusCode::CONFLICT
    );
    let handed = s
      .app
      .put(
        &s.bob.user,
        &format!("/org-recovery/requests/{request}/share"),
        json!({"sealed": b64(&for_carl)}),
      )
      .await;
    assert_eq!(handed.status, StatusCode::NO_CONTENT, "{}", handed.body);

    // Carl's device combines, unwraps the member's backup key and seals it to the request key.
    let material = s
      .app
      .get(
        &s.carl.user,
        &format!("/org-recovery/requests/{request}/material"),
      )
      .await;
    assert_eq!(material.status, StatusCode::OK, "{}", material.body);
    let carls_own = s.app.get(&s.carl.user, "/org-recovery/shares/mine").await;
    let mut shares = vec![
      OrgRecoveryShare::open_on_device(&s.carl.keys, &unb64(&carls_own.body["shares"][0]["sealed"])).unwrap(),
    ];
    for share in material.body["shares"].as_array().unwrap() {
      assert_eq!(share["from_admin_id"], s.bob.id.as_str());
      shares.push(OrgRecoveryShare::open_on_device(&s.carl.keys, &unb64(&share["sealed"])).unwrap());
    }
    let secret = combine_shares(&shares).unwrap();
    let wrapped = WrappedBackupKey::from_bytes(&unb64(&material.body["wrapped_backup_keys"])).unwrap();
    let recovered = OrgRecoveryWrapper::for_unwrapping(secret)
      .unwrap()
      .unwrap_key(crypto_user(&s.dana.id), &wrapped)
      .unwrap();
    let requester_key =
      RecoveryRequestPublicKey::from_bytes(unb64(&second.body["request_public_key"]).try_into().unwrap());
    let sealed = requester_key
      .seal_backup_key(crypto_user(&s.dana.id), &recovered)
      .unwrap();
    let done = s
      .app
      .put(
        &s.carl.user,
        &format!("/org-recovery/requests/{request}/result"),
        json!({"sealed_backup_key": b64(&sealed)}),
      )
      .await;
    assert_eq!(done.status, StatusCode::NO_CONTENT, "{}", done.body);

    // Dana's new device receives it and continues with the original backup key.
    let result = s
      .app
      .get(&s.dana.user, &format!("/org-recovery/requests/{request}/result"))
      .await;
    assert_eq!(result.status, StatusCode::OK, "{}", result.body);
    let received = request_key
      .open_backup_key(crypto_user(&s.dana.id), &unb64(&result.body["sealed_backup_key"]))
      .unwrap();
    assert_eq!(received.into_key().expose_secret(), &s.backup_bytes);
    assert_eq!(
      s.app
        .get(&s.dana.user, &format!("/org-recovery/requests/{request}"))
        .await
        .body["status"],
      "completed"
    );
    // Nothing is left open, and a finished request cannot be answered twice.
    assert_eq!(
      s.app.get(&s.bob.user, "/org-recovery/requests").await.body["requests"],
      json!([])
    );
    let again = s
      .app
      .put(
        &s.carl.user,
        &format!("/org-recovery/requests/{request}/result"),
        json!({"sealed_backup_key": b64(&sealed)}),
      )
      .await;
    assert_eq!(again.status, StatusCode::CONFLICT);

    // Dana's security log tells every step.
    let log = s.app.get(&s.dana.user, "/me/security-log").await;
    let actions: Vec<&str> = log.body["entries"]
      .as_array()
      .unwrap()
      .iter()
      .map(|entry| entry["action"].as_str().unwrap())
      .collect();
    for expected in [
      "org_recovery_requested",
      "org_recovery_approved",
      "org_recovery_material_fetched",
      "org_recovery_completed",
    ] {
      assert!(actions.contains(&expected), "{expected} missing in {actions:?}");
    }
    assert_eq!(
      actions.iter().filter(|a| **a == "org_recovery_approved").count(),
      2
    );
    let _: &OrgRecoveryPublicKey = &s.setup.public_key;
    s.app.stop().await;
  }
}

mod rules {
  use super::*;

  #[tokio::test]
  async fn one_approval_is_not_enough_and_the_wrong_people_get_nothing() {
    let Some(s) = scene().await else { return };
    let request = request_recovery(&s.app, &s.dana).await.0.id();
    let path = format!("/org-recovery/requests/{request}");
    assert_eq!(approve(&s.app, &s.bob, &request).await.status, StatusCode::OK);
    assert_eq!(
      approve(&s.app, &s.bob, &request).await.status,
      StatusCode::CONFLICT,
      "twice"
    );

    // Pending: no material for anybody, no shares from anybody.
    for admin in [&s.bob, &s.carl, &s.alice] {
      assert_eq!(
        s.app.get(&admin.user, &format!("{path}/material")).await.status,
        StatusCode::CONFLICT
      );
      assert_eq!(
        s.app
          .put(
            &admin.user,
            &format!("{path}/share"),
            json!({"sealed": b64(b"x")})
          )
          .await
          .status,
        StatusCode::CONFLICT
      );
    }
    // Members neither approve, list nor see other people's requests.
    let erin = person(&s.app, member("erin")).await;
    assert_eq!(
      approve(&s.app, &erin, &request).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      s.app.get(&erin.user, "/org-recovery/requests").await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      s.app.get(&erin.user, &path).await.error_code(),
      "recovery_request_not_found"
    );
    assert_eq!(
      s.app.get(&erin.user, "/org-recovery/shares/mine").await.status,
      StatusCode::FORBIDDEN
    );

    // After the threshold only the combining admin gets material, and nobody else a result slot.
    assert_eq!(
      approve(&s.app, &s.carl, &request).await.body["status"],
      "approved"
    );
    assert_eq!(
      s.app.get(&s.bob.user, &format!("{path}/material")).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      s.app.get(&s.alice.user, &format!("{path}/material")).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      s.app
        .put(
          &s.bob.user,
          &format!("{path}/result"),
          json!({"sealed_backup_key": b64(b"x")})
        )
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    // The combining admin keeps their own share; an admin who did not approve cannot hand one in.
    assert_eq!(
      s.app
        .put(
          &s.carl.user,
          &format!("{path}/share"),
          json!({"sealed": b64(b"x")})
        )
        .await
        .status,
      StatusCode::BAD_REQUEST
    );
    assert_eq!(
      s.app
        .put(
          &s.alice.user,
          &format!("{path}/share"),
          json!({"sealed": b64(b"x")})
        )
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    // The result is not there before the combining admin delivers it, and only Dana may ask.
    assert_eq!(
      s.app.get(&s.dana.user, &format!("{path}/result")).await.status,
      StatusCode::CONFLICT
    );
    assert_eq!(
      s.app.get(&s.bob.user, &format!("{path}/result")).await.status,
      StatusCode::FORBIDDEN
    );
    s.app.stop().await;
  }

  #[tokio::test]
  async fn admins_cannot_approve_their_own_recovery_and_one_request_is_open_at_a_time() {
    let Some(s) = scene().await else { return };
    // Bob has a key bag too.
    let wrapped = OrgRecoveryWrapper::for_wrapping(s.setup.public_key)
      .wrap_key(
        crypto_user(&s.bob.id),
        &BackupKey::generate(&mut OsRng),
        &mut OsRng,
      )
      .unwrap();
    s.app
      .put(
        &s.bob.user,
        "/me/key-bag",
        json!({"expected_version": 0, "encrypted_key_bag": b64(b"x"), "wrapped_backup_keys": b64(&wrapped.to_bytes().unwrap())}),
      )
      .await;
    let (own, _) = request_recovery(&s.app, &s.bob).await;
    assert_eq!(own.status, StatusCode::CREATED, "{}", own.body);
    assert_eq!(
      approve(&s.app, &s.bob, &own.id()).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      request_recovery(&s.app, &s.bob).await.0.status,
      StatusCode::CONFLICT
    );
    // Others can approve Bob's request.
    assert_eq!(approve(&s.app, &s.carl, &own.id()).await.status, StatusCode::OK);
    s.app.stop().await;
  }

  #[tokio::test]
  async fn requests_can_be_denied_withdrawn_and_expire() {
    let Some(s) = scene().await else { return };
    let denied = request_recovery(&s.app, &s.dana).await.0.id();
    assert_eq!(
      s.app
        .post(
          &s.dana.user,
          &format!("/org-recovery/requests/{denied}/deny"),
          json!({})
        )
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      s.app
        .post(
          &s.bob.user,
          &format!("/org-recovery/requests/{denied}/deny"),
          json!({})
        )
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    assert_eq!(
      s.app
        .get(&s.dana.user, &format!("/org-recovery/requests/{denied}"))
        .await
        .body["status"],
      "denied"
    );
    assert_eq!(
      approve(&s.app, &s.carl, &denied).await.status,
      StatusCode::CONFLICT
    );

    let withdrawn = request_recovery(&s.app, &s.dana).await.0.id();
    assert_eq!(
      s.app
        .delete(&s.bob.user, &format!("/org-recovery/requests/{withdrawn}"))
        .await
        .status,
      StatusCode::FORBIDDEN,
      "only the person withdraws"
    );
    assert_eq!(
      s.app
        .delete(&s.dana.user, &format!("/org-recovery/requests/{withdrawn}"))
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    let log = s.app.get(&s.dana.user, "/me/security-log").await.body.to_string();
    assert!(log.contains("org_recovery_denied") && log.contains("org_recovery_cancelled"));
    s.app.stop().await;

    let Some(app) = TestApp::start_with(|settings| settings.limits.org_recovery_request_ttl_secs = 0).await
    else {
      return;
    };
    let (alice, bob, carl, dana) = (
      person(&app, owner()).await,
      person(&app, admin()).await,
      person(&app, TestUser::new("carl", &["admin"])).await,
      person(&app, member("dana")).await,
    );
    let setup = create_org_recovery(2, 3, &mut OsRng).unwrap();
    let shares: Vec<Value> = [&alice, &bob, &carl]
      .iter()
      .zip(&setup.shares)
      .map(|(p, share)| {
        json!({"device_id": p.device_id, "sealed": b64(&share.seal_for_device(crypto_user(&p.id), &p.keys.public_keys().agreement_key).unwrap())})
      })
      .collect();
    app
      .put(
        &alice.user,
        "/org-recovery/shares",
        json!({"key_version": 1, "shares": shares}),
      )
      .await;
    app
      .put(
        &dana.user,
        "/me/key-bag",
        json!({"expected_version": 0, "encrypted_key_bag": b64(b"x"), "wrapped_backup_keys": b64(b"y")}),
      )
      .await;
    let request = request_recovery(&app, &dana).await.0.id();
    assert_eq!(
      approve(&app, &bob, &request).await.status,
      StatusCode::CONFLICT,
      "expired at once"
    );
    assert_eq!(
      app
        .get(&dana.user, &format!("/org-recovery/requests/{request}"))
        .await
        .body["status"],
      "expired"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn recovery_needs_setup_a_key_bag_the_policy_and_a_good_key() {
    let Some(app) = TestApp::start().await else { return };
    let (alice, dana) = (person(&app, owner()).await, person(&app, member("dana")).await);
    let key = b64(RecoveryRequestKey::generate(&mut OsRng).public_key().as_bytes());
    // No shares yet.
    let reply = app
      .post(
        &dana.user,
        "/org-recovery/requests",
        json!({"request_public_key": key}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "not set up: {}", reply.body);
    let _ = alice;
    app.stop().await;
  }
}

mod preconditions {
  use super::*;

  #[tokio::test]
  async fn a_key_bag_a_good_request_key_and_the_policy_are_needed() {
    let Some(s) = scene().await else { return };
    let erin = person(&s.app, member("erin")).await;
    let key = b64(RecoveryRequestKey::generate(&mut OsRng).public_key().as_bytes());
    // Erin has no key bag.
    let reply = s
      .app
      .post(
        &erin.user,
        "/org-recovery/requests",
        json!({"request_public_key": key}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);
    // Request keys have 32 bytes.
    for bad in [b64(&[1u8; 31]), b64(&[1u8; 33]), String::new()] {
      let reply = s
        .app
        .post(
          &s.dana.user,
          "/org-recovery/requests",
          json!({"request_public_key": bad}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    }
    // Guests are switched off by default.
    let ext = person(&s.app, guest("ext")).await;
    s.app
      .put(
        &ext.user,
        "/me/key-bag",
        json!({"expected_version": 0, "encrypted_key_bag": b64(b"x"), "wrapped_backup_keys": b64(b"y")}),
      )
      .await;
    let reply = s
      .app
      .post(
        &ext.user,
        "/org-recovery/requests",
        json!({"request_public_key": key}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
    s.app.stop().await;
  }
}

mod shares {
  use super::*;

  #[tokio::test]
  async fn only_owners_store_shares_for_admin_devices_with_a_growing_version() {
    let Some(app) = TestApp::start().await else { return };
    let (alice, bob, carl, dana) = (
      person(&app, owner()).await,
      person(&app, admin()).await,
      person(&app, TestUser::new("carl", &["admin"])).await,
      person(&app, member("dana")).await,
    );
    let share = |p: &Person| json!({"device_id": p.device_id, "sealed": b64(b"sealed")});
    let body = |version: u64, shares: Vec<Value>| json!({"key_version": version, "shares": shares});
    // Admins cannot sign; the owner can.
    assert_eq!(
      app
        .put(
          &bob.user,
          "/org-recovery/shares",
          body(1, vec![share(&alice), share(&bob)])
        )
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    // Not enough admins, a member's device, an unknown device, a duplicate, an empty set, version 0.
    for (shares, version) in [
      (vec![share(&alice)], 1),
      (vec![share(&alice), share(&dana)], 1),
      (
        vec![share(&alice), json!({"device_id": "1", "sealed": b64(b"s")})],
        1,
      ),
      (vec![share(&alice), share(&alice)], 1),
      (vec![], 1),
      (vec![share(&alice), share(&bob)], 0),
      (
        vec![share(&alice), json!({"device_id": bob.device_id, "sealed": ""})],
        1,
      ),
    ] {
      let reply = app
        .put(&alice.user, "/org-recovery/shares", body(version, shares))
        .await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    }
    assert_eq!(
      app
        .put(
          &alice.user,
          "/org-recovery/shares",
          body(1, vec![share(&alice), share(&bob), share(&carl)])
        )
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    // The version only grows.
    let stale = app
      .put(
        &alice.user,
        "/org-recovery/shares",
        body(1, vec![share(&alice), share(&bob)]),
      )
      .await;
    assert_eq!(
      (stale.status, stale.error_code()),
      (StatusCode::CONFLICT, "version_conflict")
    );
    assert_eq!(
      app
        .put(
          &alice.user,
          "/org-recovery/shares",
          body(2, vec![share(&alice), share(&bob)])
        )
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    // Carl lost his share with the rotation; Bob sees only his own.
    assert_eq!(
      app.get(&carl.user, "/org-recovery/shares/mine").await.body["shares"],
      json!([])
    );
    let mine = app.get(&bob.user, "/org-recovery/shares/mine").await;
    assert_eq!(
      (
        mine.body["key_version"].clone(),
        mine.body["shares"].as_array().unwrap().len()
      ),
      (json!(2), 1)
    );
    assert_eq!(
      app.get(&dana.user, "/org-recovery/shares/mine").await.status,
      StatusCode::FORBIDDEN
    );
    app.stop().await;
  }
}
