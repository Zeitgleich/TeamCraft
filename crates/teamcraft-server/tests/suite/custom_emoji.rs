//! Custom emoji against a real server, Postgres and the dev Garage (issue #207): admins add,
//! rename and delete them, everybody lists and downloads them, the limits hold on upload, and
//! deleting one leaves the messages that use it as they are.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use crate::common;

use common::*;
use rand_core::OsRng;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair};
use teamcraft_crypto::ids::{DeviceId as CryptoDeviceId, UserId as CryptoUserId};
use teamcraft_crypto::workspace::{WorkspaceLog, sign_log_entry};
use teamcraft_proto::{CustomEmojiDocument, CustomEmojiFormat, CustomEmojiHash, GatewayEvent};
use teamcraft_server::testing::{Reply, TestApp, TestUser};

macro_rules! app {
  () => {
    app!(|_| {})
  };
  ($customize:expr) => {
    match TestApp::start_with_storage($customize).await {
      Some(app) => app,
      None => return,
    }
  };
}

#[tokio::test]
async fn admins_add_and_everybody_lists_and_downloads() {
  let app = app!();
  let (alice, bob, carol, erin) = (owner(), admin(), member("carol"), guest("erin"));
  let carol_device = register_device(&app, &carol).await;
  let mut socket = ws::connect(&app, &carol, &carol_device).await;
  let picture = png(64, 64);

  for refused in [&carol, &erin] {
    let reply = add(&app, refused, "ship_it", &picture).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
  }
  let added = add(&app, &bob, "ship_it", &picture).await;
  assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
  assert_eq!(added.body["shortcode"], "ship_it");
  assert_eq!(added.body["format"], "image/png");
  assert_eq!(
    (added.body["width"].as_u64(), added.body["height"].as_u64()),
    (Some(64), Some(64))
  );
  assert_eq!(added.body["sha256"], hash(&picture).to_hex());
  assert_eq!(
    add(&app, &alice, "party", &png(32, 32)).await.status,
    StatusCode::CREATED
  );
  let events = ws::events(&mut socket, Duration::from_millis(500)).await;
  assert!(
    events
      .iter()
      .any(|event| matches!(event, GatewayEvent::CustomEmojiUpdate)),
    "{events:?}"
  );

  for reader in [&carol, &erin] {
    let listed = app.get(reader, "/custom-emoji").await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(shortcodes(&listed), ["party", "ship_it"]);
    assert_eq!(listed.body["limits"]["max_bytes"], 64 * 1024);
    let ship_it = &listed.body["emoji"][1];
    // The signed document travels unchanged, for the clients to verify.
    assert_eq!(ship_it["signed"], added.body["signed"]);
    let download = app
      .get(
        reader,
        &format!("/custom-emoji/{}/download", ship_it["id"].as_str().unwrap()),
      )
      .await;
    assert_eq!(download.status, StatusCode::OK, "{}", download.body);
    let fetched = app
      .http
      .get(download.body["download_url"].as_str().unwrap())
      .send()
      .await
      .unwrap();
    assert_eq!(fetched.bytes().await.unwrap().as_ref(), picture.as_slice());
  }
  app.stop().await;
}

#[tokio::test]
async fn pictures_and_names_are_checked_on_upload() {
  let app = app!(|settings| {
    settings.limits.max_custom_emoji_bytes = 2000;
    settings.limits.max_custom_emoji_side_px = 64;
    settings.limits.max_custom_emoji = 2;
  });
  let bob = admin();
  let bob_id = numeric_id(&app, &bob).await;

  let mut large = png(32, 32);
  large.resize(2001, 0);
  let large_reply = add(&app, &bob, "large", &large).await;
  assert_eq!(
    large_reply.status,
    StatusCode::PAYLOAD_TOO_LARGE,
    "{}",
    large_reply.body
  );
  for (name, picture) in [
    ("too_wide", png(65, 10)),
    ("animated", animated_png()),
    ("gif", b"GIF89a\x10\x00\x10\x00\x80\x00\x00".to_vec()),
    ("svg", b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec()),
  ] {
    let reply = add(&app, &bob, name, &picture).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{name}: {}", reply.body);
  }
  for shortcode in ["Upper", "a", "with space", "colon:", &"x".repeat(33)] {
    let reply = add(&app, &bob, shortcode, &png(16, 16)).await;
    assert_eq!(
      reply.status,
      StatusCode::BAD_REQUEST,
      "{shortcode}: {}",
      reply.body
    );
  }

  // The document must name this picture, and claim the caller as its signer.
  let picture = png(16, 16);
  let other = signed(bob_id, "other", &png(17, 17));
  let mismatch = app
    .post(
      &bob,
      "/custom-emoji",
      json!({"picture": b64(&picture), "signed": other}),
    )
    .await;
  assert_eq!(mismatch.status, StatusCode::BAD_REQUEST, "{}", mismatch.body);
  let by_someone_else = signed(bob_id + 1, "forged", &picture);
  let forged = app
    .post(
      &bob,
      "/custom-emoji",
      json!({"picture": b64(&picture), "signed": by_someone_else}),
    )
    .await;
  assert_eq!(forged.status, StatusCode::BAD_REQUEST, "{}", forged.body);

  assert_eq!(
    add(&app, &bob, "one", &png(16, 16)).await.status,
    StatusCode::CREATED
  );
  let webp = add(&app, &bob, "two", &webp(64, 64)).await;
  assert_eq!(webp.status, StatusCode::CREATED, "{}", webp.body);
  assert_eq!(webp.body["format"], "image/webp");
  let full = add(&app, &bob, "three", &png(16, 16)).await;
  assert_eq!(full.status, StatusCode::CONFLICT, "{}", full.body);
  app.stop().await;
}

#[tokio::test]
async fn names_are_unique_and_a_rename_keeps_the_picture() {
  let app = app!();
  let (bob, carol) = (admin(), member("carol"));
  let bob_id = numeric_id(&app, &bob).await;
  let picture = png(32, 32);
  let first = add(&app, &bob, "ship_it", &picture).await;
  let id = first.body["id"].as_str().unwrap().to_owned();

  let taken = add(&app, &bob, "ship_it", &png(48, 48)).await;
  assert_eq!(taken.status, StatusCode::CONFLICT, "{}", taken.body);
  assert_eq!(taken.body["error_code"], "custom_emoji_name_taken");

  let path = format!("/custom-emoji/{id}");
  let rename = json!({"signed": signed(bob_id, "shipped", &picture)});
  assert_eq!(
    app.put(&carol, &path, rename.clone()).await.status,
    StatusCode::FORBIDDEN
  );
  let renamed = app.put(&bob, &path, rename).await;
  assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);
  assert_eq!(renamed.body["shortcode"], "shipped");
  assert_eq!(renamed.body["sha256"], first.body["sha256"]);
  // A new name is signed over the same picture, not another one.
  let swapped = app
    .put(
      &bob,
      &path,
      json!({"signed": signed(bob_id, "swapped", &png(48, 48))}),
    )
    .await;
  assert_eq!(swapped.status, StatusCode::BAD_REQUEST, "{}", swapped.body);
  // The old name is free again.
  assert_eq!(
    add(&app, &bob, "ship_it", &png(48, 48)).await.status,
    StatusCode::CREATED
  );
  let missing = app
    .put(
      &bob,
      "/custom-emoji/123456789",
      json!({"signed": signed(bob_id, "gone", &picture)}),
    )
    .await;
  assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.body);
  app.stop().await;
}

#[tokio::test]
async fn deleting_an_emoji_leaves_the_messages_that_use_it() {
  let app = app!();
  let (alice, bob, carol) = (owner(), admin(), member("carol"));
  let channel = channel_with(&app, &[&carol]).await;
  let alice_device = register_device(&app, &alice).await;
  let added = add(&app, &bob, "ship_it", &png(32, 32)).await;
  let id = added.body["id"].as_str().unwrap().to_owned();
  // The server sees a ciphertext; which emoji it names is inside it.
  let message = send(&app, &alice, &channel, &alice_device, 1).await;
  let path = format!("/conversations/{channel}/messages");
  let before = app.get(&carol, &path).await;

  assert_eq!(
    app.delete(&carol, &format!("/custom-emoji/{id}")).await.status,
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    app.delete(&bob, &format!("/custom-emoji/{id}")).await.status,
    StatusCode::NO_CONTENT
  );
  assert!(shortcodes(&app.get(&carol, "/custom-emoji").await).is_empty());
  assert_eq!(
    app
      .get(&carol, &format!("/custom-emoji/{id}/download"))
      .await
      .status,
    StatusCode::NOT_FOUND
  );
  assert_eq!(
    app.delete(&bob, &format!("/custom-emoji/{id}")).await.status,
    StatusCode::NOT_FOUND
  );
  let after = app.get(&carol, &path).await;
  assert_eq!(after.status, StatusCode::OK);
  assert_eq!(after.body["messages"], before.body["messages"]);
  assert_eq!(after.body["messages"][0]["id"], message.as_str());
  app.stop().await;
}

/// Adds an emoji signed by `user`'s own (test) device.
async fn add(app: &TestApp, user: &TestUser, shortcode: &str, picture: &[u8]) -> Reply {
  let id = numeric_id(app, user).await;
  app
    .post(
      user,
      "/custom-emoji",
      json!({"picture": b64(picture), "signed": signed(id, shortcode, picture)}),
    )
    .await
}

/// A document over `picture`, signed by a fresh device of user `user_id`.
fn signed(user_id: u64, shortcode: &str, picture: &[u8]) -> Value {
  let format = if picture.starts_with(b"RIFF") {
    CustomEmojiFormat::Webp
  } else {
    CustomEmojiFormat::Png
  };
  let document = CustomEmojiDocument {
    shortcode: shortcode.to_owned(),
    sha256: hash(picture),
    format,
    signed_at_ms: 1_700_000_000_000,
  };
  let payload = serde_json::to_vec(&document).unwrap();
  let user = CryptoUserId::new(user_id);
  let identity = IdentityKeyPair::generate(&mut OsRng);
  let device = DeviceKeyPair::generate(user, CryptoDeviceId::new(user_id + 7), &mut OsRng);
  let certificate = identity.certify_device(user, &device.public_keys()).unwrap();
  let signature = sign_log_entry(
    &device,
    &certificate,
    WorkspaceLog::CustomEmoji,
    1_700_000_000_000,
    &payload,
  )
  .unwrap();
  json!({"payload": b64(&payload), "signature": b64(&signature)})
}

async fn numeric_id(app: &TestApp, user: &TestUser) -> u64 {
  user_id(app, user).await.parse().unwrap()
}

fn shortcodes(reply: &Reply) -> Vec<String> {
  reply.body["emoji"]
    .as_array()
    .unwrap()
    .iter()
    .map(|emoji| emoji["shortcode"].as_str().unwrap().to_owned())
    .collect()
}

fn hash(picture: &[u8]) -> CustomEmojiHash {
  CustomEmojiHash::from_bytes(teamcraft_crypto::attachment::content_sha256(picture))
}

/// The header of a still PNG of `width`×`height`: all the server reads.
fn png(width: u32, height: u32) -> Vec<u8> {
  png_with(width, height, &[])
}

fn animated_png() -> Vec<u8> {
  png_with(16, 16, &[b"acTL"])
}

fn png_with(width: u32, height: u32, chunks: &[&[u8; 4]]) -> Vec<u8> {
  let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
  bytes.extend_from_slice(&width.to_be_bytes());
  bytes.extend_from_slice(&height.to_be_bytes());
  bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
  for kind in chunks {
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes.extend_from_slice(*kind);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
  }
  bytes.extend_from_slice(b"\0\0\0\0IDAT\0\0\0\0");
  bytes
}

/// A lossless WebP header of `width`×`height`.
fn webp(width: u32, height: u32) -> Vec<u8> {
  let mut bytes = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0\x2f".to_vec();
  bytes.extend_from_slice(&((width - 1) | ((height - 1) << 14)).to_le_bytes());
  bytes
}

fn b64(bytes: &[u8]) -> String {
  use base64::Engine;
  base64::engine::general_purpose::STANDARD.encode(bytes)
}
