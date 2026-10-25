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
async fn abandoned_uploads_are_swept_and_free_their_slot() {
  let Some(app) =
    TestApp::start_with_storage(|settings| settings.limits.max_pending_attachments_per_user = 1).await
  else {
    return;
  };
  // A fresh user per run: the test database is shared and outlives the run.
  let nanos = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
  let erin = member(&format!("erin-{nanos}"));
  let channel = channel_with(&app, &[&erin]).await;

  // The client uploaded, then lost the network before completing, and gave up.
  let abandoned = announce(&app, &erin, &channel, BLOB.len()).await;
  assert_eq!(
    upload(&app, abandoned["upload_url"].as_str().unwrap(), BLOB).await,
    StatusCode::OK
  );
  let blocked = app
    .post(
      &erin,
      "/attachments",
      json!({"conversation_id": channel, "size_bytes": BLOB.len()}),
    )
    .await;
  assert_eq!(blocked.status, StatusCode::CONFLICT, "{}", blocked.body);

  // Once its upload URL has expired, the sweeper removes it and its object.
  let id = abandoned["id"].as_str().unwrap().parse().unwrap();
  let record = app.state.database.get_attachment(id).await.unwrap().unwrap();
  // Just after this attachment, so unfinished uploads of tests running in parallel stay.
  let cutoff = record.created_at + time::Duration::milliseconds(1);
  let swept = teamcraft_server::services::attachments::sweep_unfinished(&app.state, cutoff)
    .await
    .unwrap();
  assert!(swept >= 1);
  let store = app.state.maybe_blob_store.clone().unwrap();
  assert!(store.object_size(&record.s3_key).await.unwrap().is_none());
  assert!(app.state.database.get_attachment(id).await.unwrap().is_none());
  announce(&app, &erin, &channel, BLOB.len()).await;
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

mod multipart {
  use super::*;

  /// S3's smallest part (all but the last).
  const PART: usize = 5 * 1024 * 1024;

  macro_rules! large_app {
    () => {
      match TestApp::start_with_storage(|settings| settings.max_attachment_bytes = 64 * 1024 * 1024).await {
        Some(app) => app,
        None => return,
      }
    };
  }

  async fn announce_parts(app: &TestApp, user: &TestUser, channel: &str, size: usize) -> serde_json::Value {
    let reply = app
      .post(
        user,
        "/attachments/multipart",
        json!({"conversation_id": channel, "size_bytes": size}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    reply.body
  }

  /// Uploads one part and returns its `ETag`.
  async fn upload_part(app: &TestApp, user: &TestUser, id: &str, number: u16, bytes: &[u8]) -> String {
    let url = app
      .post(
        user,
        &format!("/attachments/{id}/parts"),
        json!({"part_number": number, "size_bytes": bytes.len()}),
      )
      .await;
    assert_eq!(url.status, StatusCode::OK, "{}", url.body);
    let response = app
      .http
      .put(url.body["upload_url"].as_str().unwrap())
      .body(bytes.to_vec())
      .send()
      .await
      .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    response.headers()["etag"].to_str().unwrap().to_owned()
  }

  fn blob(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
  }

  #[tokio::test]
  async fn parts_go_up_one_by_one_and_join_into_the_announced_blob() {
    let app = large_app!();
    let (carol, dave) = (member("carol"), member("dave"));
    let channel = channel_with(&app, &[&carol, &dave]).await;
    let blob = blob(PART + 1000);

    let created = announce_parts(&app, &carol, &channel, blob.len()).await;
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["min_part_size_bytes"], PART);
    // A part can be sent again (its answer was lost); the last ETag counts.
    upload_part(&app, &carol, &id, 1, &blob[..PART]).await;
    let first = upload_part(&app, &carol, &id, 1, &blob[..PART]).await;
    let second = upload_part(&app, &carol, &id, 2, &blob[PART..]).await;
    // The single-blob completion doesn't apply to an upload in parts.
    assert_eq!(
      app
        .post(&carol, &format!("/attachments/{id}/complete"), json!({}))
        .await
        .status,
      StatusCode::CONFLICT
    );

    let parts = json!({"parts": [{"part_number": 2, "etag": second}, {"part_number": 1, "etag": first}]});
    let completed = app
      .post(
        &carol,
        &format!("/attachments/{id}/multipart/complete"),
        parts.clone(),
      )
      .await;
    assert_eq!(completed.status, StatusCode::OK, "{}", completed.body);
    // Completing again (the answer was lost) is harmless.
    let again = app
      .post(&carol, &format!("/attachments/{id}/multipart/complete"), parts)
      .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);

    let download = app.get(&dave, &format!("/attachments/{id}/download")).await;
    assert_eq!(download.status, StatusCode::OK, "{}", download.body);
    let stored = app
      .http
      .get(download.body["download_url"].as_str().unwrap())
      .send()
      .await
      .unwrap()
      .bytes()
      .await
      .unwrap();
    assert_eq!(stored.as_ref(), blob.as_slice());
    // A completed upload can't be given up (its message carries it).
    assert_eq!(
      app.delete(&carol, &format!("/attachments/{id}")).await.status,
      StatusCode::CONFLICT
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn parts_are_checked_and_only_the_uploader_uploads_them() {
    let app = large_app!();
    let (carol, dave) = (member("carol"), member("dave"));
    let channel = channel_with(&app, &[&carol, &dave]).await;
    let created = announce_parts(&app, &carol, &channel, PART + 10).await;
    let id = created["id"].as_str().unwrap().to_owned();
    let part = |number: u64, size: u64| json!({"part_number": number, "size_bytes": size});

    for bad in [
      part(0, 10),
      part(10_001, 10),
      part(1, 0),
      part(1, (PART + 11) as u64),
    ] {
      let reply = app.post(&carol, &format!("/attachments/{id}/parts"), bad).await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    }
    let foreign = app
      .post(&dave, &format!("/attachments/{id}/parts"), part(1, 10))
      .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    let gap = json!({"parts": [{"part_number": 1, "etag": "\"a\""}, {"part_number": 3, "etag": "\"b\""}]});
    let reply = app
      .post(&carol, &format!("/attachments/{id}/multipart/complete"), gap)
      .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    // Parts the storage doesn't have can't be joined.
    let missing = json!({"parts": [{"part_number": 1, "etag": "\"0123\""}]});
    let reply = app
      .post(&carol, &format!("/attachments/{id}/multipart/complete"), missing)
      .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_cancelled_upload_is_aborted_and_frees_its_slot() {
    let Some(app) = TestApp::start_with_storage(|settings| {
      settings.max_attachment_bytes = 64 * 1024 * 1024;
      settings.limits.max_pending_attachments_per_user = 1;
    })
    .await
    else {
      return;
    };
    let nanos = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    let erin = member(&format!("erin-{nanos}"));
    let channel = channel_with(&app, &[&erin]).await;
    let created = announce_parts(&app, &erin, &channel, PART + 10).await;
    let id = created["id"].as_str().unwrap().to_owned();
    upload_part(&app, &erin, &id, 1, &blob(PART)).await;

    assert_eq!(
      app.delete(&erin, &format!("/attachments/{id}")).await.status,
      StatusCode::NO_CONTENT
    );
    // Gone: no more parts, and giving up again is harmless.
    let reply = app
      .post(
        &erin,
        &format!("/attachments/{id}/parts"),
        json!({"part_number": 2, "size_bytes": 10}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(
      app.delete(&erin, &format!("/attachments/{id}")).await.status,
      StatusCode::NO_CONTENT
    );
    announce_parts(&app, &erin, &channel, PART + 10).await;
    app.stop().await;
  }

  #[tokio::test]
  async fn the_sweeper_aborts_uploads_in_parts_nobody_continues() {
    let app = large_app!();
    let nanos = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    let erin = member(&format!("erin-{nanos}"));
    let channel = channel_with(&app, &[&erin]).await;
    let created = announce_parts(&app, &erin, &channel, PART + 10).await;
    let id = created["id"].as_str().unwrap().to_owned();
    upload_part(&app, &erin, &id, 1, &blob(PART)).await;
    let record = app
      .state
      .database
      .get_attachment(id.parse().unwrap())
      .await
      .unwrap()
      .unwrap();
    let upload_id = record.maybe_multipart_upload_id.clone().unwrap();

    // The single-upload cut-off doesn't touch an upload in parts that is still continued.
    let just_after = time::OffsetDateTime::now_utc() + time::Duration::milliseconds(1);
    teamcraft_server::services::attachments::sweep_unfinished_before(
      &app.state,
      just_after,
      record.created_at,
    )
    .await
    .unwrap();
    assert!(
      app
        .state
        .database
        .get_attachment(record.id)
        .await
        .unwrap()
        .is_some()
    );

    teamcraft_server::services::attachments::sweep_unfinished_before(&app.state, just_after, just_after)
      .await
      .unwrap();
    assert!(
      app
        .state
        .database
        .get_attachment(record.id)
        .await
        .unwrap()
        .is_none()
    );
    // The storage no longer takes parts for it.
    let store = app.state.maybe_blob_store.clone().unwrap();
    let url = store.presign_upload_part(&record.s3_key, &upload_id, 2, 10);
    let late = app.http.put(url).body(vec![0; 10]).send().await.unwrap();
    assert!(!late.status().is_success(), "{}", late.status());
    app.stop().await;
  }
}
