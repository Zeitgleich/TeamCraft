//! Attachments against a real server, Postgres and the dev Garage (skipped if either is missing).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_server::testing::{TestApp, TestUser};

macro_rules! app {
  () => {
    match TestApp::start_with_storage(|_| {}).await {
      Some(app) => app,
      None => return,
    }
  };
}

const BLOB: &[u8] = b"not really encrypted, but opaque to the server";

async fn announce(app: &TestApp, user: &TestUser, channel: &str, size: usize) -> serde_json::Value {
  let reply = app
    .post(
      user,
      "/attachments",
      json!({"conversation_id": channel, "size_bytes": size}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  reply.body
}

async fn upload(app: &TestApp, url: &str, bytes: &[u8]) -> StatusCode {
  app
    .http
    .put(url)
    .body(bytes.to_vec())
    .send()
    .await
    .unwrap()
    .status()
}

#[tokio::test]
async fn an_attachment_goes_through_announce_upload_complete_and_download() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let channel = channel_with(&app, &[&carol, &dave]).await;

  let created = announce(&app, &carol, &channel, BLOB.len()).await;
  let id = created["id"].as_str().unwrap().to_owned();
  assert!(
    created["upload_url"]
      .as_str()
      .unwrap()
      .contains("X-Amz-Signature")
  );

  // Not downloadable before the upload is confirmed.
  assert_eq!(
    app
      .get(&dave, &format!("/attachments/{id}/download"))
      .await
      .status,
    StatusCode::CONFLICT
  );
  assert_eq!(
    upload(&app, created["upload_url"].as_str().unwrap(), BLOB).await,
    StatusCode::OK
  );
  let completed = app
    .post(&carol, &format!("/attachments/{id}/complete"), json!({}))
    .await;
  assert_eq!(completed.status, StatusCode::OK, "{}", completed.body);
  assert_eq!(completed.body["is_complete"], true);
  // Completing twice is harmless.
  assert_eq!(
    app
      .post(&carol, &format!("/attachments/{id}/complete"), json!({}))
      .await
      .status,
    StatusCode::OK
  );

  let download = app.get(&dave, &format!("/attachments/{id}/download")).await;
  assert_eq!(download.status, StatusCode::OK, "{}", download.body);
  assert_eq!(download.body["size_bytes"], BLOB.len());
  let fetched = app
    .http
    .get(download.body["download_url"].as_str().unwrap())
    .send()
    .await
    .unwrap();
  assert_eq!(fetched.bytes().await.unwrap().as_ref(), BLOB);
  app.stop().await;
}

#[tokio::test]
async fn the_upload_url_only_accepts_the_announced_size() {
  let app = app!();
  let carol = member("carol");
  let channel = channel_with(&app, &[&carol]).await;

  let created = announce(&app, &carol, &channel, BLOB.len()).await;
  let url = created["upload_url"].as_str().unwrap();
  let too_small = upload(&app, url, &BLOB[..10]).await;
  assert!(
    !too_small.is_success(),
    "storage accepted a different size: {too_small}"
  );

  // Nothing was stored, so completing fails.
  let id = created["id"].as_str().unwrap();
  let completed = app
    .post(&carol, &format!("/attachments/{id}/complete"), json!({}))
    .await;
  assert_eq!(completed.status, StatusCode::CONFLICT);
  app.stop().await;
}

#[tokio::test]
async fn only_the_uploader_completes_and_only_members_download() {
  let app = app!();
  let (carol, dave, erin) = (member("carol"), member("dave"), member("erin"));
  let channel = channel_with(&app, &[&carol, &dave]).await;
  let created = announce(&app, &carol, &channel, BLOB.len()).await;
  let id = created["id"].as_str().unwrap().to_owned();
  upload(&app, created["upload_url"].as_str().unwrap(), BLOB).await;

  assert_eq!(
    app
      .post(&dave, &format!("/attachments/{id}/complete"), json!({}))
      .await
      .error_code(),
    "attachment_not_found"
  );
  app
    .post(&carol, &format!("/attachments/{id}/complete"), json!({}))
    .await;
  assert_eq!(
    app
      .get(&erin, &format!("/attachments/{id}/download"))
      .await
      .error_code(),
    "attachment_not_found"
  );
  assert_eq!(
    app.get(&dave, "/attachments/12345/download").await.error_code(),
    "attachment_not_found"
  );
  app.stop().await;
}

#[tokio::test]
async fn announcing_needs_posting_rights_and_a_sane_size() {
  let app = app!();
  let (carol, erin) = (member("carol"), member("erin"));
  let channel = channel_with(&app, &[&carol]).await;

  assert_eq!(
    app
      .post(
        &erin,
        "/attachments",
        json!({"conversation_id": channel, "size_bytes": 10})
      )
      .await
      .status,
    StatusCode::NOT_FOUND
  );
  let zero = app
    .post(
      &carol,
      "/attachments",
      json!({"conversation_id": channel, "size_bytes": 0}),
    )
    .await;
  assert_eq!(zero.status, StatusCode::BAD_REQUEST);
  let huge = app
    .post(
      &carol,
      "/attachments",
      json!({"conversation_id": channel, "size_bytes": 2 * 1024 * 1024}),
    )
    .await;
  assert_eq!(
    (huge.status, huge.error_code()),
    (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
  );
  app.stop().await;
}

#[tokio::test]
async fn attachments_from_before_a_guest_joined_stay_hidden_from_them() {
  let app = app!();
  let carol = member("carol");
  let channel = channel_with(&app, &[&carol]).await;
  let created = announce(&app, &carol, &channel, BLOB.len()).await;
  let id = created["id"].as_str().unwrap().to_owned();
  upload(&app, created["upload_url"].as_str().unwrap(), BLOB).await;
  app
    .post(&carol, &format!("/attachments/{id}/complete"), json!({}))
    .await;
  tokio::time::sleep(std::time::Duration::from_millis(20)).await;

  let ext = guest("ext");
  let ext_id = user_id(&app, &ext).await;
  let space = app.get(&owner(), "/spaces").await.body["spaces"][0]["id"]
    .as_str()
    .unwrap()
    .to_owned();
  add_space_member(&app, &owner(), &space, &ext_id, "guest").await;
  add_channel_member(&app, &owner(), &channel, &ext_id).await;
  assert_eq!(
    app
      .get(&ext, &format!("/attachments/{id}/download"))
      .await
      .error_code(),
    "attachment_not_found"
  );
  app.stop().await;
}

#[tokio::test]
async fn without_configured_storage_the_endpoints_report_unavailable() {
  let Some(app) = TestApp::start().await else { return };
  let carol = member("carol");
  let channel = channel_with(&app, &[&carol]).await;
  let reply = app
    .post(
      &carol,
      "/attachments",
      json!({"conversation_id": channel, "size_bytes": 10}),
    )
    .await;
  assert_eq!(
    (reply.status, reply.error_code()),
    (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
  );
  app.stop().await;
}
