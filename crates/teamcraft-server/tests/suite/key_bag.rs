//! Key bag storage against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_server::testing::TestApp;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

fn bag(expected: u64, content: &str) -> serde_json::Value {
  json!({"expected_version": expected, "encrypted_key_bag": content, "wrapped_backup_keys": "d3JhcHBlZA=="})
}

#[tokio::test]
async fn a_key_bag_is_created_read_back_and_versioned() {
  let app = app!();
  let carol = member("carol");
  assert_eq!(
    app.get(&carol, "/me/key-bag").await.error_code(),
    "key_bag_not_found"
  );

  let created = app.put(&carol, "/me/key-bag", bag(0, "Ym9uZTE=")).await;
  assert_eq!(created.status, StatusCode::OK, "{}", created.body);
  assert_eq!(created.body["version"], 1);
  let updated = app.put(&carol, "/me/key-bag", bag(1, "Ym9uZTI=")).await;
  assert_eq!(updated.body["version"], 2);

  let read = app.get(&carol, "/me/key-bag").await;
  assert_eq!(read.body["encrypted_key_bag"], "Ym9uZTI=");
  assert_eq!(read.body["wrapped_backup_keys"], "d3JhcHBlZA==");
  // Key bags are private to their owner.
  assert_eq!(
    app.get(&member("dave"), "/me/key-bag").await.status,
    StatusCode::NOT_FOUND
  );
  app.stop().await;
}

#[tokio::test]
async fn stale_versions_are_rejected_without_changing_the_bag() {
  let app = app!();
  let carol = member("carol");
  app.put(&carol, "/me/key-bag", bag(0, "Ym9uZTE=")).await;
  app.put(&carol, "/me/key-bag", bag(1, "Ym9uZTI=")).await;

  let stale = app.put(&carol, "/me/key-bag", bag(1, "c3RhbGU=")).await;
  assert_eq!(
    (stale.status, stale.error_code()),
    (StatusCode::CONFLICT, "version_conflict")
  );
  assert!(stale.body["message"].as_str().unwrap().contains('2'));
  assert_eq!(
    stale.body["current_version"], 2,
    "the version is a structured field"
  );
  assert!(stale.body.get("current_epoch").is_none());
  let again_create = app.put(&carol, "/me/key-bag", bag(0, "c3RhbGU=")).await;
  assert_eq!(again_create.error_code(), "version_conflict");
  let future = app.put(&carol, "/me/key-bag", bag(9, "c3RhbGU=")).await;
  assert_eq!(future.error_code(), "version_conflict");
  assert_eq!(
    app.get(&carol, "/me/key-bag").await.body["encrypted_key_bag"],
    "Ym9uZTI="
  );
  app.stop().await;
}

#[tokio::test]
async fn of_concurrent_writers_with_one_version_exactly_one_wins() {
  let app = app!();
  let carol = member("carol");
  app.put(&carol, "/me/key-bag", bag(0, "Ym9uZTE=")).await;

  let writers = (0..10).map(|n| {
    let (app, carol) = (&app, &carol);
    async move {
      app
        .put(carol, "/me/key-bag", bag(1, &format!("{:0>4}", n + 1000)))
        .await
    }
  });
  let replies = futures_util::future::join_all(writers).await;
  assert_eq!(replies.iter().filter(|r| r.status == StatusCode::OK).count(), 1);
  assert_eq!(
    replies
      .iter()
      .filter(|r| r.error_code_or("") == "version_conflict")
      .count(),
    9
  );
  assert_eq!(app.get(&carol, "/me/key-bag").await.body["version"], 2);
  app.stop().await;
}

#[tokio::test]
async fn empty_and_oversized_bags_are_rejected() {
  let Some(app) = TestApp::start_with(|s| s.limits.max_key_bag_bytes = 16).await else {
    return;
  };
  let carol = member("carol");
  let empty = app.put(&carol, "/me/key-bag", bag(0, "")).await;
  assert_eq!(empty.status, StatusCode::BAD_REQUEST);
  let big = app.put(&carol, "/me/key-bag", bag(0, &"QUJD".repeat(20))).await;
  assert_eq!(
    (big.status, big.error_code()),
    (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
  );
  app.stop().await;
}

#[tokio::test]
async fn the_users_devices_hear_about_a_new_key_bag() {
  use std::time::Duration;

  use teamcraft_proto::GatewayEvent;

  let app = app!();
  let (alice, bob) = (owner(), member("bob"));
  let device = register_device(&app, &alice).await;
  let bob_device = register_device(&app, &bob).await;
  let mut socket = ws::connect(&app, &alice, &device).await;
  let mut other = ws::connect(&app, &bob, &bob_device).await;
  let quiet = Duration::from_millis(400);
  ws::events(&mut socket, quiet).await;
  ws::events(&mut other, quiet).await;

  let put = app.put(&alice, "/me/key-bag", bag(0, "Ym9uZTE=")).await;
  assert_eq!(put.status, StatusCode::OK, "{}", put.body);
  let events = ws::events(&mut socket, quiet).await;
  assert!(
    events
      .iter()
      .any(|event| matches!(event, GatewayEvent::KeyBagUpdate { version: 1 })),
    "{events:?}"
  );
  let bobs = ws::events(&mut other, quiet).await;
  assert!(
    !bobs
      .iter()
      .any(|event| matches!(event, GatewayEvent::KeyBagUpdate { .. }))
  );
  app.stop().await;
}

/// With the signed policy's `recovery.is_passphrase_allowed` off (e2ee.md §6.5.8), a member keeps
/// uploading their unchanged legacy passphrase wrap but can't set a new one, neither in a key bag
/// nor with a new identity; a recovery kit is always accepted, and guests are exempt.
#[tokio::test]
async fn with_passphrases_off_members_set_no_new_passphrase() {
  let app = app!();
  let carol = member("carol");
  let (kit, old_passphrase, new_passphrase) = wrap_lists();
  let first = app
    .put(&carol, "/me/key-bag", wrapped_bag(0, &old_passphrase))
    .await;
  assert_eq!(first.status, StatusCode::OK, "{}", first.body);

  publish_policy_without_passphrases(&app).await;

  let unchanged = app
    .put(&carol, "/me/key-bag", wrapped_bag(1, &old_passphrase))
    .await;
  assert_eq!(unchanged.status, StatusCode::OK, "{}", unchanged.body);
  let changed = app
    .put(&carol, "/me/key-bag", wrapped_bag(2, &new_passphrase))
    .await;
  assert_eq!(
    (changed.status, changed.error_code()),
    (StatusCode::FORBIDDEN, "recovery_passphrase_not_allowed")
  );
  let malformed = app.put(&carol, "/me/key-bag", bag(2, "Ym9uZTE=")).await;
  assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
  let replaced = app.put(&carol, "/me/key-bag", wrapped_bag(2, &kit)).await;
  assert_eq!(replaced.status, StatusCode::OK, "{}", replaced.body);
  // Once replaced, the old passphrase can't come back either.
  let back = app
    .put(&carol, "/me/key-bag", wrapped_bag(3, &old_passphrase))
    .await;
  assert_eq!(back.error_code(), "recovery_passphrase_not_allowed");

  let dave = member("dave");
  let identity = |wraps: &str| {
    json!({
      "public_key": "a2V5MQ==",
      "signature": "c2ln",
      "key_bag": {"encrypted_key_bag": "YmFn", "wrapped_backup_keys": wraps},
    })
  };
  let refused = app
    .put(&dave, "/me/identity-key", identity(&new_passphrase))
    .await;
  assert_eq!(refused.error_code(), "recovery_passphrase_not_allowed");
  let enrolled = app.put(&dave, "/me/identity-key", identity(&kit)).await;
  assert_eq!(enrolled.status, StatusCode::OK, "{}", enrolled.body);

  // A meeting guest's bag is wrapped with a random secret nobody keeps.
  let visitor = guest("visitor");
  let guest_bag = app
    .put(&visitor, "/me/key-bag", wrapped_bag(0, &new_passphrase))
    .await;
  assert_eq!(guest_bag.status, StatusCode::OK, "{}", guest_bag.body);
  app.stop().await;
}

fn wrapped_bag(expected: u64, wrapped_backup_keys: &str) -> serde_json::Value {
  json!({"expected_version": expected, "encrypted_key_bag": "Ym9uZTE=", "wrapped_backup_keys": wrapped_backup_keys})
}

/// Base64 wrap lists: a recovery kit's, and two different passphrases'.
fn wrap_lists() -> (String, String, String) {
  use base64::Engine;
  use teamcraft_crypto::backup::{
    Argon2Settings, BackupKey, BackupKeyWrapper, PassphraseWrapper, RecoveryKeyWrapper, WrappedBackupKey,
    encode_wrapped_key_list,
  };
  use teamcraft_crypto::ids::UserId;
  use teamcraft_crypto::recovery_key::RecoveryKey;
  use teamcraft_crypto::secret::SecretBytes;

  let cheap = Argon2Settings {
    memory_kib: 64,
    iterations: 1,
    parallelism: 1,
  };
  let user = UserId::new(1);
  let backup_key = BackupKey::generate(&mut rand_core::OsRng);
  let passphrase = |text: &str| {
    PassphraseWrapper::new(SecretBytes::new(text.as_bytes().to_vec()), cheap)
      .unwrap()
      .wrap_key(user, &backup_key, &mut rand_core::OsRng)
      .unwrap()
  };
  let kit = RecoveryKeyWrapper::new(&RecoveryKey::generate(&mut rand_core::OsRng))
    .wrap_key(user, &backup_key, &mut rand_core::OsRng)
    .unwrap();
  let encode = |wraps: &[WrappedBackupKey]| {
    base64::engine::general_purpose::STANDARD.encode(encode_wrapped_key_list(wraps).unwrap())
  };
  (
    encode(&[kit]),
    encode(&[passphrase("the old passphrase")]),
    encode(&[passphrase("a brand new passphrase")]),
  )
}

/// The owner publishes the default policy with passphrases switched off (the server checks the
/// owner and the chain, not the signature).
async fn publish_policy_without_passphrases(app: &TestApp) {
  use base64::Engine;
  use teamcraft_proto::{PolicyDocument, RecoveryPolicy, WorkspacePolicy};

  let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
  let alice = owner();
  let device = register_device(app, &alice).await;
  let document = PolicyDocument {
    version: 1,
    policy: WorkspacePolicy {
      recovery: RecoveryPolicy {
        is_passphrase_allowed: false,
      },
      ..WorkspacePolicy::default()
    },
  };
  let reply = app
    .post(
      &alice,
      "/workspace/policy",
      json!({
        "expected_version": 0,
        "payload": b64(&serde_json::to_vec(&document).unwrap()),
        "signature": b64(b"sig"),
        "signer_device_id": device,
      }),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}
