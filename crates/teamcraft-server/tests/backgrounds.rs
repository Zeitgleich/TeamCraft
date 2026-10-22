//! A user's own call backgrounds against a real server, Postgres and the dev Garage.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_server::testing::TestApp;

macro_rules! app {
  ($customize:expr) => {
    match TestApp::start_with_storage($customize).await {
      Some(app) => app,
      None => return,
    }
  };
}

#[tokio::test]
async fn a_background_goes_through_announce_upload_complete_download_and_delete() {
  let app = app!(|_| {});
  let (carol, dave) = (member("carol"), member("dave"));
  let encrypted = b"opaque ciphertext of a picture".to_vec();

  let announced = app
    .post(&carol, "/me/backgrounds", json!({"size_bytes": encrypted.len()}))
    .await;
  assert_eq!(announced.status, StatusCode::CREATED, "{}", announced.body);
  let id = announced.body["id"].as_str().unwrap().to_owned();
  let url = announced.body["upload_url"].as_str().unwrap().to_owned();

  // Not uploaded yet: completing and downloading are refused.
  let early = app
    .post(&carol, &format!("/me/backgrounds/{id}/complete"), json!({}))
    .await;
  assert_eq!(early.status, StatusCode::CONFLICT);
  assert_eq!(
    app
      .get(&carol, &format!("/me/backgrounds/{id}/download"))
      .await
      .status,
    StatusCode::CONFLICT
  );

  let put = app.http.put(&url).body(encrypted.clone()).send().await.unwrap();
  assert_eq!(put.status(), StatusCode::OK);
  let completed = app
    .post(&carol, &format!("/me/backgrounds/{id}/complete"), json!({}))
    .await;
  assert_eq!(completed.status, StatusCode::NO_CONTENT, "{}", completed.body);

  // Somebody else's background doesn't exist for them.
  let foreign = app.get(&dave, &format!("/me/backgrounds/{id}/download")).await;
  assert_eq!(foreign.status, StatusCode::NOT_FOUND);
  assert_eq!(foreign.body["error_code"], "background_not_found");
  assert_eq!(
    app.delete(&dave, &format!("/me/backgrounds/{id}")).await.status,
    StatusCode::NOT_FOUND
  );

  let download = app.get(&carol, &format!("/me/backgrounds/{id}/download")).await;
  assert_eq!(download.status, StatusCode::OK, "{}", download.body);
  assert_eq!(download.body["size_bytes"], encrypted.len());
  let fetched = app
    .http
    .get(download.body["download_url"].as_str().unwrap())
    .send()
    .await
    .unwrap();
  assert_eq!(fetched.bytes().await.unwrap().as_ref(), encrypted.as_slice());

  assert_eq!(
    app.delete(&carol, &format!("/me/backgrounds/{id}")).await.status,
    StatusCode::NO_CONTENT
  );
  assert_eq!(
    app
      .get(&carol, &format!("/me/backgrounds/{id}/download"))
      .await
      .status,
    StatusCode::NOT_FOUND
  );
  app.stop().await;
}

#[tokio::test]
async fn sizes_and_counts_are_limited() {
  let app = app!(|settings| {
    settings.limits.max_background_bytes = 1000;
    settings.limits.max_backgrounds_per_user = 2;
  });
  let carol = member("carol");
  assert_eq!(
    app
      .post(&carol, "/me/backgrounds", json!({"size_bytes": 0}))
      .await
      .status,
    StatusCode::BAD_REQUEST
  );
  assert_eq!(
    app
      .post(&carol, "/me/backgrounds", json!({"size_bytes": 1001}))
      .await
      .status,
    StatusCode::PAYLOAD_TOO_LARGE
  );
  for _ in 0..2 {
    let announced = app
      .post(&carol, "/me/backgrounds", json!({"size_bytes": 10}))
      .await;
    assert_eq!(announced.status, StatusCode::CREATED);
  }
  assert_eq!(
    app
      .post(&carol, "/me/backgrounds", json!({"size_bytes": 10}))
      .await
      .status,
    StatusCode::CONFLICT
  );

  // A wrong size is not completed.
  let dave = member("dave");
  let announced = app
    .post(&dave, "/me/backgrounds", json!({"size_bytes": 10}))
    .await;
  let id = announced.body["id"].as_str().unwrap().to_owned();
  let url = announced.body["upload_url"].as_str().unwrap();
  let put = app.http.put(url).body(vec![0u8; 9]).send().await.unwrap();
  assert_ne!(put.status(), StatusCode::OK, "the length is signed");
  assert_eq!(
    app
      .post(&dave, &format!("/me/backgrounds/{id}/complete"), json!({}))
      .await
      .status,
    StatusCode::CONFLICT
  );
  app.stop().await;
}
