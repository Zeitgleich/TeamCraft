//! Profiles and avatars against a real server, Postgres and the dev Garage.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_proto::GatewayEvent;
use teamcraft_server::testing::TestApp;

macro_rules! app {
  () => {
    match TestApp::start_with_storage(|_| {}).await {
      Some(app) => app,
      None => return,
    }
  };
}

mod fields {
  use super::*;

  #[tokio::test]
  async fn a_profile_is_replaced_as_a_whole_and_visible_to_people_who_may_see_the_user() {
    let app = app!();
    let (carol, dave, erin) = (member("carol"), member("dave"), guest("erin"));
    let carol_id = user_id(&app, &carol).await;
    channel_with(&app, &[&carol, &dave]).await;

    assert_eq!(app.get(&carol, "/me/profile").await.body["has_avatar"], false);
    let put = app
      .put(
        &carol,
        "/me/profile",
        json!({
          "title": "  Staff Engineer ",
          "department": "Platform",
          "phone": "+49 30 123456",
          "time_zone": "Europe/Berlin",
          "working_hours": {"weekdays": ["mon", "tue", "fri"], "start_minute": 540, "end_minute": 1020},
        }),
      )
      .await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.body);
    assert_eq!(put.body["title"], "Staff Engineer");
    assert_eq!(
      put.body["working_hours"]["weekdays"],
      json!(["mon", "tue", "fri"])
    );

    let seen = app.get(&dave, &format!("/users/{carol_id}/profile")).await;
    assert_eq!(seen.status, StatusCode::OK);
    assert_eq!(seen.body["department"], "Platform");
    assert_eq!(seen.body["time_zone"], "Europe/Berlin");
    // A guest who shares no conversation with Carol does not find her.
    assert_eq!(
      app.get(&erin, &format!("/users/{carol_id}/profile")).await.status,
      StatusCode::NOT_FOUND
    );

    // A second PUT replaces everything: what is left out is cleared.
    let replaced = app.put(&carol, "/me/profile", json!({"title": "Lead"})).await;
    assert_eq!(replaced.body["title"], "Lead");
    assert!(replaced.body.get("department").is_none());
    assert!(replaced.body.get("working_hours").is_none());
    app.stop().await;
  }

  #[tokio::test]
  async fn invalid_fields_are_refused() {
    let app = app!();
    let carol = member("carol");
    for body in [
      json!({"title": "x".repeat(101)}),
      json!({"title": "line\nbreak"}),
      json!({"phone": "call me maybe"}),
      json!({"time_zone": "Europe/Berlin; DROP"}),
      json!({"time_zone": "../etc/passwd"}),
      json!({"working_hours": {"weekdays": [], "start_minute": 0, "end_minute": 60}}),
      json!({"working_hours": {"weekdays": ["mon"], "start_minute": 600, "end_minute": 500}}),
      json!({"working_hours": {"weekdays": ["mon"], "start_minute": 0, "end_minute": 2000}}),
      json!({"working_hours": {"weekdays": ["someday"], "start_minute": 0, "end_minute": 60}}),
    ] {
      let reply = app.put(&carol, "/me/profile", body.clone()).await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{body}: {}", reply.body);
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn people_sharing_a_conversation_are_told_about_changes() {
    let app = app!();
    let (carol, dave) = (member("carol"), member("dave"));
    let carol_id = user_id(&app, &carol).await;
    channel_with(&app, &[&carol, &dave]).await;
    let device = register_device(&app, &dave).await;
    let mut socket = ws::connect(&app, &dave, &device).await;
    ws::events(&mut socket, Duration::from_millis(300)).await;

    app.put(&carol, "/me/profile", json!({"title": "Lead"})).await;
    let events = ws::events(&mut socket, Duration::from_millis(500)).await;
    assert!(events.iter().any(|event| matches!(
      event,
      GatewayEvent::ProfileUpdate { user_id } if user_id.to_string() == carol_id
    )));
    app.stop().await;
  }
}

mod avatars {
  use super::*;

  async fn upload(app: &TestApp, url: &str, content_type: &str, bytes: &[u8]) -> StatusCode {
    app
      .http
      .put(url)
      .header("content-type", content_type)
      .body(bytes.to_vec())
      .send()
      .await
      .unwrap()
      .status()
  }

  #[tokio::test]
  async fn an_avatar_goes_through_announce_upload_complete_and_download() {
    let app = app!();
    let (carol, dave) = (member("carol"), member("dave"));
    let carol_id = user_id(&app, &carol).await;
    channel_with(&app, &[&carol, &dave]).await;
    let image = b"\x89PNG not really an image but the right size";

    assert_eq!(
      app.get(&dave, &format!("/users/{carol_id}/avatar")).await.status,
      StatusCode::NOT_FOUND
    );
    let announced = app
      .post(
        &carol,
        "/me/avatar",
        json!({"content_type": "image/png", "size_bytes": image.len()}),
      )
      .await;
    assert_eq!(announced.status, StatusCode::CREATED, "{}", announced.body);
    let url = announced.body["upload_url"].as_str().unwrap().to_owned();

    // Completing before the upload fails; so does uploading a different type.
    assert_eq!(
      app.post(&carol, "/me/avatar/complete", json!({})).await.status,
      StatusCode::CONFLICT
    );
    assert_ne!(
      upload(&app, &url, "text/html", image).await,
      StatusCode::OK,
      "the type is signed"
    );
    assert_eq!(upload(&app, &url, "image/png", image).await, StatusCode::OK);
    let completed = app.post(&carol, "/me/avatar/complete", json!({})).await;
    assert_eq!(completed.status, StatusCode::OK, "{}", completed.body);
    assert_eq!(completed.body["has_avatar"], true);

    let avatar = app.get(&dave, &format!("/users/{carol_id}/avatar")).await;
    assert_eq!(avatar.status, StatusCode::OK, "{}", avatar.body);
    assert_eq!(avatar.body["content_type"], "image/png");
    assert_eq!(avatar.body["size_bytes"], image.len());
    let fetched = app
      .http
      .get(avatar.body["download_url"].as_str().unwrap())
      .send()
      .await
      .unwrap();
    assert_eq!(fetched.status(), StatusCode::OK);
    assert_eq!(fetched.headers()["content-type"], "image/png");
    assert_eq!(fetched.bytes().await.unwrap().as_ref(), image);

    assert_eq!(
      app.delete(&carol, "/me/avatar").await.status,
      StatusCode::NO_CONTENT
    );
    assert_eq!(
      app.get(&dave, &format!("/users/{carol_id}/avatar")).await.status,
      StatusCode::NOT_FOUND
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_wrong_size_is_not_accepted_and_limits_apply() {
    let app = app!();
    let carol = member("carol");
    let announced = app
      .post(
        &carol,
        "/me/avatar",
        json!({"content_type": "image/jpeg", "size_bytes": 100}),
      )
      .await;
    let url = announced.body["upload_url"].as_str().unwrap().to_owned();
    // The signature covers the announced length: the storage refuses another one.
    assert_ne!(upload(&app, &url, "image/jpeg", &[1u8; 99]).await, StatusCode::OK);
    assert_eq!(
      app.post(&carol, "/me/avatar/complete", json!({})).await.status,
      StatusCode::CONFLICT
    );

    for (body, status) in [
      (
        json!({"content_type": "image/gif", "size_bytes": 10}),
        StatusCode::BAD_REQUEST,
      ),
      (
        json!({"content_type": "image/png", "size_bytes": 0}),
        StatusCode::BAD_REQUEST,
      ),
      (
        json!({"content_type": "image/png", "size_bytes": 2 * 1024 * 1024}),
        StatusCode::PAYLOAD_TOO_LARGE,
      ),
    ] {
      assert_eq!(
        app.post(&carol, "/me/avatar", body.clone()).await.status,
        status,
        "{body}"
      );
    }
    app.stop().await;
  }
}
