//! Space pictures against a real server, Postgres and the dev Garage: who may set them, what is
//! accepted, and how they are served.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::io::Cursor;

use common::*;
use image::{ImageBuffer, ImageFormat, Rgba};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use teamcraft_server::testing::{TestApp, TestUser};

const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;

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

mod serving {
  use super::*;

  #[tokio::test]
  async fn a_space_admin_sets_a_picture_that_members_download_as_png() {
    let app = app!();
    let (alice, carol, dave) = (owner(), member("carol"), member("dave"));
    let space = create_space(&app, &alice, "Project A", "invite_only").await;
    let carol_id = user_id(&app, &carol).await;
    let dave_id = user_id(&app, &dave).await;
    add_space_member(&app, &alice, &space, &carol_id, "space_admin").await;
    add_space_member(&app, &alice, &space, &dave_id, "member").await;
    let path = format!("/spaces/{space}/image");

    let (status, body) = put_image(
      &app,
      &carol,
      &path,
      "image/jpeg",
      &picture(300, 300, ImageFormat::Jpeg),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let version = body["image"]["version"].as_str().unwrap().to_owned();
    assert!(body["image"]["size_bytes"].as_u64().unwrap() > 0, "{body}");
    let listed = app.get(&dave, "/spaces").await;
    assert_eq!(listed.body["spaces"][0]["image"]["version"], version.as_str());

    // Re-encoded as PNG and served with a type, nosniff, a private cache and the version as ETag.
    let response = raw_get(&app, &dave, &path, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    assert_eq!(headers["content-type"], "image/png");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["cache-control"], "private, no-cache");
    let etag = headers["etag"].to_str().unwrap().to_owned();
    assert_eq!(etag, format!("\"{version}\""));
    let bytes = response.bytes().await.unwrap();
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 300);

    let revalidated = raw_get(&app, &dave, &path, Some(&etag)).await;
    assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);

    // A new picture has a new version; the old ETag no longer matches.
    let (status, body) = put_image(
      &app,
      &carol,
      &path,
      "image/png",
      &picture(64, 64, ImageFormat::Png),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(body["image"]["version"], version.as_str());
    assert_eq!(
      raw_get(&app, &dave, &path, Some(&etag)).await.status(),
      StatusCode::OK
    );

    let removed = app.delete(&carol, &path).await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.body);
    assert!(removed.body.get("image").is_none(), "{}", removed.body);
    let missing = app.get(&dave, &path).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.error_code(), "space_image_not_found");
    assert_eq!(app.delete(&carol, &path).await.status, StatusCode::NOT_FOUND);
    app.stop().await;
  }

  #[tokio::test]
  async fn people_who_cannot_see_the_space_do_not_get_its_picture() {
    let app = app!();
    let (alice, erin, frank) = (owner(), member("erin"), guest("frank"));
    let hidden = create_space(&app, &alice, "Project A", "invite_only").await;
    let open = create_space(&app, &alice, "Project B", "open").await;
    for space in [&hidden, &open] {
      let path = format!("/spaces/{space}/image");
      let (status, body) =
        put_image(&app, &alice, &path, "image/png", &picture(8, 8, ImageFormat::Png)).await;
      assert_eq!(status, StatusCode::OK, "{body}");
    }

    let hidden_path = format!("/spaces/{hidden}/image");
    assert_eq!(app.get(&erin, &hidden_path).await.status, StatusCode::NOT_FOUND);
    assert_eq!(app.get(&frank, &hidden_path).await.status, StatusCode::NOT_FOUND);
    // An open space shows its picture to employees who may join it, not to guests.
    let open_path = format!("/spaces/{open}/image");
    assert_eq!(
      raw_get(&app, &erin, &open_path, None).await.status(),
      StatusCode::OK
    );
    assert_eq!(app.get(&frank, &open_path).await.status, StatusCode::NOT_FOUND);
    let anonymous = app.call(Method::GET, None, &open_path, None).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    app.stop().await;
  }
}

mod permissions {
  use super::*;

  #[tokio::test]
  async fn only_people_who_manage_the_space_change_its_picture() {
    let app = app!();
    let (alice, dave, erin) = (owner(), member("dave"), member("erin"));
    let space = create_space(&app, &alice, "Project A", "open").await;
    let dave_id = user_id(&app, &dave).await;
    add_space_member(&app, &alice, &space, &dave_id, "member").await;
    let path = format!("/spaces/{space}/image");
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
    let unknown = put_image(&app, &alice, "/spaces/1/image", "image/png", &png).await;
    assert_eq!(unknown.0, StatusCode::NOT_FOUND);
    let audit = app.get(&alice, "/admin/audit?action=space_image_set").await;
    assert_eq!(
      audit.body["entries"].as_array().map(Vec::len),
      Some(1),
      "{}",
      audit.body
    );
    app.stop().await;
  }
}

mod validation {
  use super::*;

  #[tokio::test]
  async fn svg_mislabelled_and_broken_files_are_refused() {
    let app = app!();
    let alice = owner();
    let space = create_space(&app, &alice, "Project A", "invite_only").await;
    let path = format!("/spaces/{space}/image");
    let png = picture(16, 16, ImageFormat::Png);
    for (content_type, body) in [
      ("image/svg+xml", SVG.to_vec()),
      ("image/png", SVG.to_vec()),
      ("image/jpeg", png.clone()),
      ("image/gif", b"GIF89a\x01\0\x01\0".to_vec()),
      ("text/html", b"<html><script></script></html>".to_vec()),
      ("image/png", png[..png.len() / 2].to_vec()),
    ] {
      let (status, reply) = put_image(&app, &alice, &path, content_type, &body).await;
      assert_eq!(status, StatusCode::BAD_REQUEST, "{content_type}: {reply}");
    }
    assert!(
      app
        .get(&alice, &format!("/spaces/{space}"))
        .await
        .body
        .get("image")
        .is_none()
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn pictures_over_the_limit_are_refused() {
    let app = app!(|settings| settings.limits.max_space_image_bytes = 2000);
    let alice = owner();
    let space = create_space(&app, &alice, "Project A", "invite_only").await;
    let path = format!("/spaces/{space}/image");
    // An empty body is a malformed request, not a too large one.
    let (status, reply) = put_image(&app, &alice, &path, "image/png", &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
    let noisy = noise_png(200);
    assert!(noisy.len() > 2000);
    let (status, reply) = put_image(&app, &alice, &path, "image/png", &noisy).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{reply}");
    let (status, reply) = put_image(&app, &alice, &path, "image/png", &picture(4, 4, ImageFormat::Png)).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    app.stop().await;
  }
}

async fn put_image(
  app: &TestApp,
  user: &TestUser,
  path: &str,
  content_type: &str,
  body: &[u8],
) -> (StatusCode, Value) {
  let response = app
    .http
    .put(format!("{}/api/v1{path}", app.base_url))
    .bearer_auth(app.token(user))
    .header("Content-Type", content_type)
    .body(body.to_vec())
    .send()
    .await
    .unwrap();
  let status = response.status();
  (status, response.json().await.unwrap_or(Value::Null))
}

async fn raw_get(app: &TestApp, user: &TestUser, path: &str, maybe_etag: Option<&str>) -> reqwest::Response {
  let mut request = app
    .http
    .get(format!("{}/api/v1{path}", app.base_url))
    .bearer_auth(app.token(user));
  if let Some(etag) = maybe_etag {
    request = request.header("If-None-Match", etag);
  }
  request.send().await.unwrap()
}

fn picture(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
  let mut out = Cursor::new(Vec::new());
  let buffer = ImageBuffer::from_fn(width, height, |x, y| {
    Rgba([(x % 256) as u8, (y % 256) as u8, 80, 255])
  });
  if format == ImageFormat::Jpeg {
    image::DynamicImage::ImageRgba8(buffer)
      .to_rgb8()
      .write_to(&mut out, format)
      .unwrap();
  } else {
    buffer.write_to(&mut out, format).unwrap();
  }
  out.into_inner()
}

/// A PNG that doesn't compress: pseudo-random pixels.
fn noise_png(side: u32) -> Vec<u8> {
  let mut seed = 0x2545_f491_u32;
  let buffer = ImageBuffer::from_fn(side, side, |_, _| {
    seed ^= seed << 13;
    seed ^= seed >> 17;
    seed ^= seed << 5;
    let [r, g, b, _] = seed.to_le_bytes();
    Rgba([r, g, b, 255])
  });
  let mut out = Cursor::new(Vec::new());
  buffer.write_to(&mut out, ImageFormat::Png).unwrap();
  out.into_inner()
}
