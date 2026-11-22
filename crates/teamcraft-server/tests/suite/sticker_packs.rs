//! Sticker packs against a real server, Postgres and the dev Garage (issue #2132): admins upload
//! pictures and save packs, everybody lists them and downloads the pictures, the document's rules
//! and the limits hold, versions only move forward, and deleting a pack leaves messages alone.
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
use teamcraft_proto::{
  CustomEmojiFormat, CustomEmojiHash, GatewayEvent, StickerEntry, StickerPackDocument, StickerPackKey,
};
use teamcraft_server::testing::{Reply, TestApp, TestUser};

const PACK: &str = "0123456789abcdef0123456789abcdef";

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
async fn admins_save_and_everybody_lists_and_downloads() {
  let app = app!();
  let (bob, carol, erin) = (admin(), member("carol"), guest("erin"));
  let bob_id = numeric_id(&app, &bob).await;
  let carol_device = register_device(&app, &carol).await;
  let mut socket = ws::connect(&app, &carol, &carol_device).await;
  let (wave, ok) = (png(128, 128), png(256, 200));

  for refused in [&carol, &erin] {
    assert_eq!(upload(&app, refused, &wave).await.status, StatusCode::FORBIDDEN);
  }
  let uploaded = upload(&app, &bob, &wave).await;
  assert_eq!(uploaded.status, StatusCode::OK, "{}", uploaded.body);
  assert_eq!(uploaded.body["sha256"], hash(&wave).to_hex());
  assert_eq!(uploaded.body["width"], 128);
  // Uploading it again changes nothing.
  assert_eq!(upload(&app, &bob, &wave).await.body, uploaded.body);
  assert_eq!(upload(&app, &bob, &ok).await.status, StatusCode::OK);

  let document = signed(bob_id, 1, "Team", &[("wave", &wave), ("ok", &ok)]);
  assert_eq!(
    save(&app, &carol, PACK, document.clone()).await.status,
    StatusCode::FORBIDDEN
  );
  let saved = save(&app, &bob, PACK, document.clone()).await;
  assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
  assert_eq!(saved.body["version"], 1);
  assert_eq!(saved.body["name"], "Team");
  let events = ws::events(&mut socket, Duration::from_millis(500)).await;
  assert!(
    events
      .iter()
      .any(|event| matches!(event, GatewayEvent::StickerPacksUpdate)),
    "{events:?}"
  );

  for reader in [&carol, &erin] {
    let listed = app.get(reader, "/sticker-packs").await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["packs"][0]["pack"], PACK);
    // The signed document travels unchanged, for the clients to verify.
    assert_eq!(listed.body["packs"][0]["signed"], document);
    assert_eq!(listed.body["limits"]["max_bytes"], 256 * 1024);
    let download = app
      .get(
        reader,
        &format!("/sticker-pictures/{}/download", hash(&ok).to_hex()),
      )
      .await;
    assert_eq!(download.status, StatusCode::OK, "{}", download.body);
    let fetched = app
      .http
      .get(download.body["download_url"].as_str().unwrap())
      .send()
      .await
      .unwrap();
    assert_eq!(fetched.bytes().await.unwrap().as_ref(), ok.as_slice());
  }
  app.stop().await;
}

#[tokio::test]
async fn pictures_documents_and_limits_are_checked() {
  let app = app!(|settings| {
    settings.limits.max_sticker_bytes = 2000;
    settings.limits.max_sticker_side_px = 64;
    settings.limits.max_sticker_packs = 1;
    settings.limits.max_stickers_per_pack = 2;
  });
  let bob = admin();
  let bob_id = numeric_id(&app, &bob).await;

  let mut large = png(32, 32);
  large.resize(2001, 0);
  assert_eq!(
    upload(&app, &bob, &large).await.status,
    StatusCode::PAYLOAD_TOO_LARGE
  );
  for picture in [
    png(65, 10),
    animated_png(),
    b"GIF89a\x10\x00\x10\x00\x80\x00\x00".to_vec(),
    b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec(),
  ] {
    let reply = upload(&app, &bob, &picture).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
  }

  let (one, two, three) = (png(16, 16), png(17, 17), png(18, 18));
  for picture in [&one, &two, &three] {
    assert_eq!(upload(&app, &bob, picture).await.status, StatusCode::OK);
  }
  // A picture that isn't uploaded, too many stickers, a duplicate or invalid name, no alt text,
  // another pack in the path, a wrong format, a signer who isn't the caller.
  let not_uploaded = png(19, 19);
  let mut wrong_format = document(1, "Team", &[("one", &one)]);
  wrong_format.stickers[0].format = CustomEmojiFormat::Webp;
  let mut no_alt = document(1, "Team", &[("one", &one)]);
  no_alt.stickers[0].alt = " ".into();
  for (case, body, path) in [
    (
      "missing picture",
      signed(bob_id, 1, "Team", &[("x", &not_uploaded)]),
      PACK,
    ),
    (
      "too many",
      signed(
        bob_id,
        1,
        "Team",
        &[("one", &one), ("two", &two), ("three", &three)],
      ),
      PACK,
    ),
    (
      "duplicate",
      signed(bob_id, 1, "Team", &[("one", &one), ("one", &two)]),
      PACK,
    ),
    ("invalid name", signed(bob_id, 1, "Team", &[("One", &one)]), PACK),
    ("no name", signed(bob_id, 1, " ", &[("one", &one)]), PACK),
    ("no alt", sign(bob_id, &no_alt), PACK),
    ("wrong format", sign(bob_id, &wrong_format), PACK),
    ("version 0", signed(bob_id, 0, "Team", &[("one", &one)]), PACK),
    (
      "other pack",
      signed(bob_id, 1, "Team", &[("one", &one)]),
      "ffffffffffffffffffffffffffffffff",
    ),
    ("forged", signed(bob_id + 1, 1, "Team", &[("one", &one)]), PACK),
  ] {
    let reply = save(&app, &bob, path, body).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{case}: {}", reply.body);
  }
  assert_eq!(
    save(&app, &bob, "not-a-key", signed(bob_id, 1, "Team", &[]))
      .await
      .status,
    StatusCode::BAD_REQUEST
  );

  let ok = save(
    &app,
    &bob,
    PACK,
    signed(bob_id, 1, "Team", &[("one", &one), ("two", &two)]),
  )
  .await;
  assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
  let full = save(
    &app,
    &bob,
    "11111111111111111111111111111111",
    signed_for("11111111111111111111111111111111", bob_id, 1, "More", &[]),
  )
  .await;
  assert_eq!(full.status, StatusCode::CONFLICT, "{}", full.body);
  app.stop().await;
}

#[tokio::test]
async fn versions_only_move_forward() {
  let app = app!();
  let bob = admin();
  let bob_id = numeric_id(&app, &bob).await;
  let picture = png(32, 32);
  upload(&app, &bob, &picture).await;
  assert_eq!(
    save(&app, &bob, PACK, signed(bob_id, 2, "Team", &[("wave", &picture)]))
      .await
      .status,
    StatusCode::OK
  );
  for stale in [1, 2] {
    let reply = save(&app, &bob, PACK, signed(bob_id, stale, "Old", &[])).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);
    assert_eq!(reply.body["error_code"], "sticker_pack_stale");
  }
  let renamed = save(&app, &bob, PACK, signed(bob_id, 3, "Renamed", &[])).await;
  assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);
  assert_eq!(renamed.body["name"], "Renamed");
  app.stop().await;
}

#[tokio::test]
async fn deleting_a_pack_leaves_the_messages_that_use_it() {
  let app = app!();
  let (alice, bob, carol) = (owner(), admin(), member("carol"));
  let bob_id = numeric_id(&app, &bob).await;
  let channel = channel_with(&app, &[&carol]).await;
  let alice_device = register_device(&app, &alice).await;
  let picture = png(32, 32);
  upload(&app, &bob, &picture).await;
  save(&app, &bob, PACK, signed(bob_id, 1, "Team", &[("wave", &picture)])).await;
  // The server sees a ciphertext; which sticker it names is inside it.
  let message = send(&app, &alice, &channel, &alice_device, 1).await;
  let path = format!("/conversations/{channel}/messages");
  let before = app.get(&carol, &path).await;

  let pack_path = format!("/sticker-packs/{PACK}");
  assert_eq!(app.delete(&carol, &pack_path).await.status, StatusCode::FORBIDDEN);
  assert_eq!(app.delete(&bob, &pack_path).await.status, StatusCode::NO_CONTENT);
  assert_eq!(app.delete(&bob, &pack_path).await.status, StatusCode::NOT_FOUND);
  let listed = app.get(&carol, "/sticker-packs").await;
  assert_eq!(listed.body["packs"], json!([]));
  let after = app.get(&carol, &path).await;
  assert_eq!(after.body["messages"], before.body["messages"]);
  assert_eq!(after.body["messages"][0]["id"], message.as_str());
  app.stop().await;
}

async fn upload(app: &TestApp, user: &TestUser, picture: &[u8]) -> Reply {
  app
    .post(user, "/sticker-pictures", json!({"picture": b64(picture)}))
    .await
}

async fn save(app: &TestApp, user: &TestUser, pack: &str, signed: Value) -> Reply {
  app
    .put(user, &format!("/sticker-packs/{pack}"), json!({"signed": signed}))
    .await
}

fn document(version: u64, name: &str, stickers: &[(&str, &[u8])]) -> StickerPackDocument {
  document_for(PACK, version, name, stickers)
}

fn document_for(pack: &str, version: u64, name: &str, stickers: &[(&str, &[u8])]) -> StickerPackDocument {
  StickerPackDocument {
    pack: StickerPackKey::from_hex(pack).unwrap(),
    version,
    name: name.to_owned(),
    stickers: stickers
      .iter()
      .map(|(name, picture)| StickerEntry {
        name: (*name).to_owned(),
        alt: format!("Sticker {name}"),
        sha256: hash(picture),
        format: CustomEmojiFormat::Png,
      })
      .collect(),
    signed_at_ms: 1_700_000_000_000,
  }
}

fn signed(user_id: u64, version: u64, name: &str, stickers: &[(&str, &[u8])]) -> Value {
  sign(user_id, &document(version, name, stickers))
}

fn signed_for(pack: &str, user_id: u64, version: u64, name: &str, stickers: &[(&str, &[u8])]) -> Value {
  sign(user_id, &document_for(pack, version, name, stickers))
}

/// The document, signed by a fresh device of user `user_id`.
fn sign(user_id: u64, document: &StickerPackDocument) -> Value {
  let payload = serde_json::to_vec(document).unwrap();
  let user = CryptoUserId::new(user_id);
  let identity = IdentityKeyPair::generate(&mut OsRng);
  let device = DeviceKeyPair::generate(user, CryptoDeviceId::new(user_id + 7), &mut OsRng);
  let certificate = identity.certify_device(user, &device.public_keys()).unwrap();
  let signature = sign_log_entry(
    &device,
    &certificate,
    WorkspaceLog::StickerPack,
    document.version,
    &payload,
  )
  .unwrap();
  json!({"payload": b64(&payload), "signature": b64(&signature)})
}

async fn numeric_id(app: &TestApp, user: &TestUser) -> u64 {
  user_id(app, user).await.parse().unwrap()
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

fn b64(bytes: &[u8]) -> String {
  use base64::Engine;
  base64::engine::general_purpose::STANDARD.encode(bytes)
}
