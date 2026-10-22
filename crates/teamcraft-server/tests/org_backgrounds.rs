//! Organization call backgrounds against a real server, Postgres and the dev Garage: admins
//! upload, everybody downloads, the signed policy may only list uploaded pictures, and listed
//! pictures can't be deleted.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::{PolicyDocument, WorkspacePolicy};
use teamcraft_server::testing::{Reply, TestApp, TestUser};

macro_rules! app {
  () => {
    match TestApp::start_with_storage(|_| {}).await {
      Some(app) => app,
      None => return,
    }
  };
}

const PICTURE: &[u8] = b"\xff\xd8\xff not really a JPEG, but bytes of the announced size";

#[tokio::test]
async fn admins_upload_and_everybody_downloads() {
  let app = app!();
  let (bob, carol, erin) = (admin(), member("carol"), guest("erin"));
  let announce = json!({"size_bytes": PICTURE.len()});
  assert_eq!(
    app
      .post(&carol, "/admin/backgrounds", announce.clone())
      .await
      .status,
    StatusCode::FORBIDDEN
  );
  let id = upload(&app, &bob).await;

  for reader in [&carol, &erin] {
    let download = app.get(reader, &format!("/org-backgrounds/{id}/download")).await;
    assert_eq!(download.status, StatusCode::OK, "{}", download.body);
    let fetched = app
      .http
      .get(download.body["download_url"].as_str().unwrap())
      .send()
      .await
      .unwrap();
    assert_eq!(fetched.bytes().await.unwrap().as_ref(), PICTURE);
  }
  assert_eq!(
    app
      .delete(&carol, &format!("/admin/backgrounds/{id}"))
      .await
      .status,
    StatusCode::FORBIDDEN
  );
  app.stop().await;
}

#[tokio::test]
async fn the_policy_lists_only_uploaded_pictures_and_listed_ones_stay() {
  let app = app!();
  let alice = owner();
  let device = register_device(&app, &alice).await;
  let id = upload(&app, &alice).await;

  let missing = policy(1, &[("123456789", PICTURE.len())]);
  assert_eq!(
    append_policy(&app, &alice, &device, 0, &missing).await.status,
    StatusCode::BAD_REQUEST
  );
  let wrong_size = policy(1, &[(&id, PICTURE.len() + 1)]);
  assert_eq!(
    append_policy(&app, &alice, &device, 0, &wrong_size).await.status,
    StatusCode::BAD_REQUEST
  );
  let listed = policy(1, &[(&id, PICTURE.len())]);
  let appended = append_policy(&app, &alice, &device, 0, &listed).await;
  assert_eq!(appended.status, StatusCode::CREATED, "{}", appended.body);

  let refused = app.delete(&alice, &format!("/admin/backgrounds/{id}")).await;
  assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.body);
  let unlisted = policy(2, &[]);
  assert_eq!(
    append_policy(&app, &alice, &device, 1, &unlisted).await.status,
    StatusCode::CREATED
  );
  assert_eq!(
    app
      .delete(&alice, &format!("/admin/backgrounds/{id}"))
      .await
      .status,
    StatusCode::NO_CONTENT
  );
  assert_eq!(
    app
      .get(&alice, &format!("/org-backgrounds/{id}/download"))
      .await
      .status,
    StatusCode::NOT_FOUND
  );
  app.stop().await;
}

/// Announces, uploads (typed `image/jpeg`) and completes a picture; returns its id.
async fn upload(app: &TestApp, admin: &TestUser) -> String {
  let announced = app
    .post(admin, "/admin/backgrounds", json!({"size_bytes": PICTURE.len()}))
    .await;
  assert_eq!(announced.status, StatusCode::CREATED, "{}", announced.body);
  let id = announced.body["id"].as_str().unwrap().to_owned();
  let url = announced.body["upload_url"].as_str().unwrap();
  let put = app
    .http
    .put(url)
    .header("content-type", "image/jpeg")
    .body(PICTURE.to_vec())
    .send()
    .await
    .unwrap();
  assert_eq!(put.status(), StatusCode::OK);
  let completed = app
    .post(admin, &format!("/admin/backgrounds/{id}/complete"), json!({}))
    .await;
  assert_eq!(completed.status, StatusCode::NO_CONTENT, "{}", completed.body);
  id
}

/// A policy document whose call backgrounds list `images` (id, size) as required.
fn policy(version: u64, images: &[(&str, usize)]) -> Vec<u8> {
  let mut value = serde_json::to_value(PolicyDocument {
    version,
    policy: WorkspacePolicy::default(),
  })
  .unwrap();
  let images: Vec<Value> = images
    .iter()
    .map(|(id, size)| json!({"id": id, "name": "Lobby", "sha256": b64(&[7; 32]), "size_bytes": size}))
    .collect();
  let mode = if images.is_empty() { "free" } else { "required" };
  value["policy"]["call_backgrounds"] = json!({"mode": mode, "images": images, "guests_exempt": true});
  serde_json::to_vec(&value).unwrap()
}

async fn append_policy(app: &TestApp, user: &TestUser, device: &str, expected: u64, payload: &[u8]) -> Reply {
  app
    .post(
      user,
      "/workspace/policy",
      json!({"expected_version": expected, "payload": b64(payload), "signature": b64(b"sig"), "signer_device_id": device}),
    )
    .await
}

fn b64(bytes: &[u8]) -> String {
  use base64::Engine;
  base64::engine::general_purpose::STANDARD.encode(bytes)
}
