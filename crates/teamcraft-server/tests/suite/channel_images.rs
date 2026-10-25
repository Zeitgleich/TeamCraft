//! Channel pictures against a real server, Postgres and the dev Garage: who may set them, who sees
//! them and how they are served (the image rules themselves are tested with the space pictures).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;
use crate::space_images::{picture, put_image, raw_get};

use common::*;
use image::ImageFormat;
use reqwest::StatusCode;
use teamcraft_server::testing::TestApp;

macro_rules! app {
  () => {
    match TestApp::start_with_storage(|_| {}).await {
      Some(app) => app,
      None => return,
    }
  };
}

#[tokio::test]
async fn a_channel_admin_sets_a_picture_that_members_download_as_png() {
  let app = app!();
  let (alice, dave) = (owner(), member("dave"));
  let space = create_space(&app, &alice, "Project A", "open").await;
  let dave_id = user_id(&app, &dave).await;
  add_space_member(&app, &alice, &space, &dave_id, "member").await;
  let channel = create_channel(&app, &alice, &space, "ops", "public").await;
  add_channel_member(&app, &alice, &channel, &dave_id).await;
  let path = format!("/conversations/{channel}/image");

  let (status, body) = put_image(
    &app,
    &alice,
    &path,
    "image/jpeg",
    &picture(300, 300, ImageFormat::Jpeg),
  )
  .await;
  assert_eq!(status, StatusCode::OK, "{body}");
  let version = body["image"]["version"].as_str().unwrap().to_owned();
  let listed = app.get(&dave, &format!("/conversations/{channel}")).await;
  assert_eq!(listed.body["image"]["version"], version.as_str());

  let response = raw_get(&app, &dave, &path, None).await;
  assert_eq!(response.status(), StatusCode::OK);
  let etag = response.headers()["etag"].to_str().unwrap().to_owned();
  assert_eq!(etag, format!("\"{version}\""));
  assert_eq!(response.headers()["content-type"], "image/png");
  assert!(response.bytes().await.unwrap().starts_with(b"\x89PNG\r\n\x1a\n"));
  assert_eq!(
    raw_get(&app, &dave, &path, Some(&etag)).await.status(),
    StatusCode::NOT_MODIFIED
  );

  let removed = app.delete(&alice, &path).await;
  assert_eq!(removed.status, StatusCode::OK, "{}", removed.body);
  assert!(removed.body.get("image").is_none(), "{}", removed.body);
  let missing = app.get(&dave, &path).await;
  assert_eq!(missing.status, StatusCode::NOT_FOUND);
  assert_eq!(missing.error_code(), "channel_image_not_found");
  app.stop().await;
}

#[tokio::test]
async fn only_people_who_manage_the_channel_change_its_picture() {
  let app = app!();
  let (alice, dave, erin) = (owner(), member("dave"), member("erin"));
  let space = create_space(&app, &alice, "Project A", "open").await;
  let dave_id = user_id(&app, &dave).await;
  add_space_member(&app, &alice, &space, &dave_id, "member").await;
  let channel = create_channel(&app, &alice, &space, "ops", "public").await;
  add_channel_member(&app, &alice, &channel, &dave_id).await;
  let path = format!("/conversations/{channel}/image");
  let png = picture(16, 16, ImageFormat::Png);

  for user in [&dave, &erin] {
    let (status, body) = put_image(&app, user, &path, "image/png", &png).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
  }
  let (status, body) = put_image(&app, &alice, &path, "image/png", &png).await;
  assert_eq!(status, StatusCode::OK, "{body}");
  for user in [&dave, &erin] {
    assert_eq!(app.delete(user, &path).await.status, StatusCode::FORBIDDEN);
  }
  let audit = app.get(&alice, "/admin/audit?action=channel_image_set").await;
  assert_eq!(
    audit.body["entries"].as_array().map(Vec::len),
    Some(1),
    "{}",
    audit.body
  );
  app.stop().await;
}

#[tokio::test]
async fn people_who_cannot_see_a_private_channel_do_not_get_its_picture() {
  let app = app!();
  let (alice, erin) = (owner(), member("erin"));
  let space = create_space(&app, &alice, "Project A", "open").await;
  let channel = create_channel(&app, &alice, &space, "secret", "private").await;
  let path = format!("/conversations/{channel}/image");
  let (status, body) = put_image(&app, &alice, &path, "image/png", &picture(8, 8, ImageFormat::Png)).await;
  assert_eq!(status, StatusCode::OK, "{body}");
  assert_eq!(app.get(&erin, &path).await.status, StatusCode::NOT_FOUND);
  app.stop().await;
}
