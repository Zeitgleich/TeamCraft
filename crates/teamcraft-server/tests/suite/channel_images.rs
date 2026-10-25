//! Channel pictures against a real server, Postgres and the dev Garage: who may set them, who sees
//! them and how they are served (the image rules themselves are tested with the space pictures).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;
use crate::space_images::{noise_png, picture, put_image, raw_get};

use common::*;
use image::ImageFormat;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_server::testing::TestApp;

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
  let erin_id = user_id(&app, &erin).await;
  add_space_member(&app, &alice, &space, &erin_id, "member").await;
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

mod who_may_change_it {
  use super::*;

  #[tokio::test]
  async fn a_channel_admin_who_is_not_an_instance_owner_may_and_a_moderator_may_not() {
    let app = app!();
    let (alice, dave, eve) = (owner(), member("dave"), member("eve"));
    let space = create_space(&app, &alice, "Project A", "open").await;
    let (dave_id, eve_id) = (user_id(&app, &dave).await, user_id(&app, &eve).await);
    add_space_member(&app, &alice, &space, &dave_id, "member").await;
    add_space_member(&app, &alice, &space, &eve_id, "member").await;
    // Dave creates the channel, so he is its admin without any workspace role.
    let channel = create_channel(&app, &dave, &space, "ops", "public").await;
    add_channel_member(&app, &dave, &channel, &eve_id).await;
    let role = app
      .patch(
        &dave,
        &format!("/conversations/{channel}/members/{eve_id}"),
        json!({"role": "moderator"}),
      )
      .await;
    assert_eq!(role.status, StatusCode::NO_CONTENT, "{}", role.body);
    let path = format!("/conversations/{channel}/image");
    let png = picture(16, 16, ImageFormat::Png);

    let (status, body) = put_image(&app, &eve, &path, "image/png", &png).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "moderator: {body}");
    assert_eq!(app.delete(&eve, &path).await.status, StatusCode::FORBIDDEN);
    let (status, body) = put_image(&app, &dave, &path, "image/png", &png).await;
    assert_eq!(status, StatusCode::OK, "channel admin: {body}");
    app.stop().await;
  }

  #[tokio::test]
  async fn a_guest_member_may_not() {
    let app = app!();
    let (alice, frank) = (owner(), guest("frank"));
    let space = create_space(&app, &alice, "Project A", "open").await;
    let frank_id = user_id(&app, &frank).await;
    add_space_member(&app, &alice, &space, &frank_id, "guest").await;
    let channel = create_channel(&app, &alice, &space, "ops", "public").await;
    add_channel_member(&app, &alice, &channel, &frank_id).await;
    let path = format!("/conversations/{channel}/image");
    let (status, body) = put_image(&app, &frank, &path, "image/png", &picture(8, 8, ImageFormat::Png)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    app.stop().await;
  }

  #[tokio::test]
  async fn a_space_admin_may_in_a_public_channel_but_not_in_a_private_one_they_are_not_in() {
    let app = app!();
    let (alice, gina) = (owner(), member("gina"));
    let space = create_space(&app, &alice, "Project A", "open").await;
    let gina_id = user_id(&app, &gina).await;
    add_space_member(&app, &alice, &space, &gina_id, "space_admin").await;
    let public = create_channel(&app, &alice, &space, "ops", "public").await;
    let private = create_channel(&app, &alice, &space, "secret", "private").await;
    let png = picture(8, 8, ImageFormat::Png);

    let (status, body) = put_image(
      &app,
      &gina,
      &format!("/conversations/{public}/image"),
      "image/png",
      &png,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = put_image(
      &app,
      &gina,
      &format!("/conversations/{private}/image"),
      "image/png",
      &png,
    )
    .await;
    assert!(
      matches!(status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
      "{status}: {body}"
    );
    app.stop().await;
  }
}

mod what_has_a_picture {
  use super::*;

  #[tokio::test]
  async fn direct_messages_have_none() {
    let app = app!();
    let (alice, dave) = (owner(), member("dave"));
    let dave_id = user_id(&app, &dave).await;
    let dm = app.post(&alice, "/dms", json!({"user_id": dave_id})).await.id();
    let path = format!("/conversations/{dm}/image");
    let (status, body) = put_image(&app, &alice, &path, "image/png", &picture(8, 8, ImageFormat::Png)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(app.delete(&alice, &path).await.status, StatusCode::BAD_REQUEST);
    app.stop().await;
  }

  #[tokio::test]
  async fn an_archived_channel_keeps_its_picture() {
    let app = app!();
    let alice = owner();
    let space = create_space(&app, &alice, "Project A", "open").await;
    let channel = create_channel(&app, &alice, &space, "ops", "public").await;
    let path = format!("/conversations/{channel}/image");
    let png = picture(8, 8, ImageFormat::Png);
    let (status, body) = put_image(&app, &alice, &path, "image/png", &png).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let archived = app
      .patch(
        &alice,
        &format!("/conversations/{channel}"),
        json!({"is_archived": true}),
      )
      .await;
    assert_eq!(archived.status, StatusCode::OK, "{}", archived.body);
    let (status, body) = put_image(&app, &alice, &path, "image/png", &png).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(app.delete(&alice, &path).await.status, StatusCode::CONFLICT);
    assert_eq!(raw_get(&app, &alice, &path, None).await.status(), StatusCode::OK);
    app.stop().await;
  }

  #[tokio::test]
  async fn pictures_over_the_limit_are_refused() {
    let app = app!(|settings| settings.limits.max_space_image_bytes = 2000);
    let alice = owner();
    let space = create_space(&app, &alice, "Project A", "open").await;
    let channel = create_channel(&app, &alice, &space, "ops", "public").await;
    let path = format!("/conversations/{channel}/image");
    let noisy = noise_png(200);
    assert!(noisy.len() > 2000);
    let (status, body) = put_image(&app, &alice, &path, "image/png", &noisy).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    let (status, body) = put_image(&app, &alice, &path, "image/png", &picture(4, 4, ImageFormat::Png)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    app.stop().await;
  }
}
