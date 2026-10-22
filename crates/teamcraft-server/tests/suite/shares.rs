//! Share links for external download: creating, limits, revoking, the public endpoints and the
//! download page (needs the dev Garage for the files, like the attachment tests).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::PathBuf;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_db::testing::sqlx;
use teamcraft_server::testing::{Reply, TestApp, TestUser};

const BLOB: &[u8] = b"ciphertext the server cannot read";

struct Scene {
  app: TestApp,
  carol: TestUser,
  dave: TestUser,
  attachment: String,
  channel: String,
}

async fn scene_with(customize: impl FnOnce(&mut teamcraft_server::state::Settings)) -> Option<Scene> {
  let app = TestApp::start_with_storage(customize).await?;
  let (carol, dave) = (member("carol"), member("dave"));
  let channel = channel_with(&app, &[&carol, &dave]).await;
  let attachment = upload(&app, &carol, &channel, BLOB).await;
  Some(Scene {
    app,
    carol,
    dave,
    attachment,
    channel,
  })
}

async fn scene() -> Option<Scene> {
  scene_with(|_| {}).await
}

/// Announces, uploads and completes an attachment; returns its ID.
async fn upload(app: &TestApp, user: &TestUser, channel: &str, bytes: &[u8]) -> String {
  let created = app
    .post(
      user,
      "/attachments",
      json!({"conversation_id": channel, "size_bytes": bytes.len()}),
    )
    .await;
  assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
  let put = app
    .http
    .put(created.body["upload_url"].as_str().unwrap())
    .body(bytes.to_vec())
    .send()
    .await
    .unwrap();
  assert_eq!(put.status(), StatusCode::OK);
  let id = created.body["id"].as_str().unwrap().to_owned();
  let done = app
    .post(user, &format!("/attachments/{id}/complete"), json!({}))
    .await;
  assert_eq!(done.status, StatusCode::OK, "{}", done.body);
  id
}

async fn share(s: &Scene, body: Value) -> Reply {
  s.app.post(&s.carol, "/shares", body).await
}

/// A request without credentials, as the public download page makes it.
async fn public(app: &TestApp, method: reqwest::Method, path: &str) -> (StatusCode, Value) {
  let response = app
    .http
    .request(method, format!("{}/api/v1{path}", app.base_url))
    .send()
    .await
    .unwrap();
  let status = response.status();
  (status, response.json().await.unwrap_or(Value::Null))
}

mod links {
  use super::*;

  #[tokio::test]
  async fn a_link_serves_the_ciphertext_until_it_is_used_up_expired_or_revoked() {
    let Some(s) = scene().await else { return };
    let created = share(
      &s,
      json!({"attachment_id": s.attachment, "max_downloads": 2, "is_password_protected": true}),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.body["id"].as_str().unwrap().to_owned();
    assert!(id.len() >= 40, "unguessable: {id}");
    assert_eq!(created.body["download_count"], 0);
    assert_eq!(created.body["is_password_protected"], true);

    // The public page learns the size and that a password is needed, without counting.
    for _ in 0..3 {
      let (status, info) = public(&s.app, reqwest::Method::GET, &format!("/shares/{id}")).await;
      assert_eq!(status, StatusCode::OK, "{info}");
      assert_eq!(info["size_bytes"], BLOB.len());
      assert_eq!(info["is_password_protected"], true);
    }
    assert_eq!(
      s.app.get(&s.carol, "/shares").await.body["links"][0]["download_count"],
      0
    );

    for expected in [1, 2] {
      let (status, download) = public(&s.app, reqwest::Method::POST, &format!("/shares/{id}/download")).await;
      assert_eq!(status, StatusCode::OK, "{download}");
      let bytes = s
        .app
        .http
        .get(download["download_url"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
      assert_eq!(bytes.as_ref(), BLOB);
      let listed = s.app.get(&s.carol, "/shares").await;
      assert_eq!(listed.body["links"][0]["download_count"], expected);
      assert!(listed.body["links"][0]["last_downloaded_at"].is_i64());
    }
    // Used up: one answer for everything that is not available.
    let (status, body) = public(&s.app, reqwest::Method::POST, &format!("/shares/{id}/download")).await;
    assert_eq!(
      (status, body["error_code"].clone()),
      (StatusCode::GONE, json!("share_unavailable"))
    );
    assert_eq!(
      public(&s.app, reqwest::Method::GET, &format!("/shares/{id}"))
        .await
        .0,
      StatusCode::GONE
    );
    s.app.stop().await;
  }

  #[tokio::test]
  async fn expired_revoked_and_unknown_links_are_not_distinguishable() {
    let Some(s) = scene().await else { return };
    let expiring = share(&s, json!({"attachment_id": s.attachment})).await.body["id"]
      .as_str()
      .unwrap()
      .to_owned();
    let revoked = share(&s, json!({"attachment_id": s.attachment})).await.body["id"]
      .as_str()
      .unwrap()
      .to_owned();
    let alive = share(&s, json!({"attachment_id": s.attachment})).await.body["id"]
      .as_str()
      .unwrap()
      .to_owned();

    sqlx::query("UPDATE share_links SET expires_at = now() - interval '1 minute' WHERE id = $1")
      .bind(&expiring)
      .execute(s.app.sql_pool())
      .await
      .unwrap();
    // Only the creator revokes; another member sees nothing; an admin may.
    assert_eq!(
      s.app.delete(&s.dave, &format!("/shares/{revoked}")).await.status,
      StatusCode::NOT_FOUND
    );
    assert_eq!(
      s.app.delete(&s.carol, &format!("/shares/{revoked}")).await.status,
      StatusCode::NO_CONTENT
    );
    assert_eq!(
      s.app.delete(&s.carol, &format!("/shares/{revoked}")).await.status,
      StatusCode::CONFLICT
    );
    assert_eq!(
      s.app.delete(&admin(), &format!("/shares/{alive}")).await.status,
      StatusCode::NO_CONTENT
    );

    let mut answers = Vec::new();
    for id in [
      expiring.as_str(),
      revoked.as_str(),
      alive.as_str(),
      "unknown-link",
      "x",
    ] {
      for method in [reqwest::Method::GET, reqwest::Method::POST] {
        let path = if method == reqwest::Method::GET {
          format!("/shares/{id}")
        } else {
          format!("/shares/{id}/download")
        };
        let (status, body) = public(&s.app, method, &path).await;
        answers.push((status, body["error_code"].clone(), body["message"].clone()));
      }
    }
    assert!(answers.iter().all(|a| a == &answers[0]), "{answers:?}");
    assert_eq!(answers[0].0, StatusCode::GONE);
    // The list shows them as they are.
    let listed = s.app.get(&s.carol, "/shares").await;
    assert_eq!(listed.body["links"].as_array().unwrap().len(), 3);
    let revoked_row = listed.body["links"]
      .as_array()
      .unwrap()
      .iter()
      .find(|l| l["id"] == revoked.as_str())
      .unwrap();
    assert_eq!(revoked_row["is_revoked"], true);
    s.app.stop().await;
  }

  #[tokio::test]
  async fn expiry_and_download_limits_follow_the_instance_settings() {
    let Some(s) = scene_with(|settings| {
      settings.shares.max_expiry_days = 10;
      settings.shares.default_expiry_days = 3;
      settings.shares.max_downloads_cap = 5;
      settings.shares.public_base_url = "https://chat.example.com/".into();
    })
    .await
    else {
      return;
    };
    let now_ms = time::OffsetDateTime::now_utc().unix_timestamp() * 1000;
    let day = 86_400_000_i64;
    let default = share(&s, json!({"attachment_id": s.attachment})).await;
    assert_eq!(default.status, StatusCode::CREATED, "{}", default.body);
    let expires = default.body["expires_at"].as_i64().unwrap();
    assert!((expires - (now_ms + 3 * day)).abs() < 60_000);
    assert_eq!(default.body["max_downloads"], 5, "links get the cap");
    let id = default.body["id"].as_str().unwrap();
    assert_eq!(default.body["url"], format!("https://chat.example.com/f/{id}"));

    for body in [
      json!({"attachment_id": s.attachment, "expires_at": now_ms + 11 * day}),
      json!({"attachment_id": s.attachment, "expires_at": now_ms - 1000}),
      json!({"attachment_id": s.attachment, "max_downloads": 0}),
      json!({"attachment_id": s.attachment, "max_downloads": 6}),
    ] {
      assert_eq!(
        share(&s, body.clone()).await.status,
        StatusCode::BAD_REQUEST,
        "{body}"
      );
    }
    let fine = share(
      &s,
      json!({"attachment_id": s.attachment, "expires_at": now_ms + 9 * day, "max_downloads": 1}),
    )
    .await;
    assert_eq!(fine.status, StatusCode::CREATED);
    s.app.stop().await;
  }

  #[tokio::test]
  async fn the_number_of_live_links_per_person_is_limited() {
    let Some(s) = scene_with(|settings| settings.shares.max_links_per_user = 2).await else {
      return;
    };
    for _ in 0..2 {
      assert_eq!(
        share(&s, json!({"attachment_id": s.attachment})).await.status,
        StatusCode::CREATED
      );
    }
    assert_eq!(
      share(&s, json!({"attachment_id": s.attachment})).await.status,
      StatusCode::CONFLICT
    );
    let first = s.app.get(&s.carol, "/shares").await.body["links"][0]["id"]
      .as_str()
      .unwrap()
      .to_owned();
    s.app.delete(&s.carol, &format!("/shares/{first}")).await;
    assert_eq!(
      share(&s, json!({"attachment_id": s.attachment})).await.status,
      StatusCode::CREATED
    );
    s.app.stop().await;
  }
}

mod who_may_share {
  use super::*;

  #[tokio::test]
  async fn only_people_who_can_read_the_file_and_may_share_make_links() {
    let Some(s) = scene().await else { return };
    // Another member of the channel can share what they can read.
    let by_dave = s
      .app
      .post(&s.dave, "/shares", json!({"attachment_id": s.attachment}))
      .await;
    assert_eq!(by_dave.status, StatusCode::CREATED, "{}", by_dave.body);
    // Strangers cannot even tell the file exists.
    let erin = member("erin");
    let stranger = s
      .app
      .post(&erin, "/shares", json!({"attachment_id": s.attachment}))
      .await;
    assert_eq!(
      (stranger.status, stranger.error_code()),
      (StatusCode::NOT_FOUND, "attachment_not_found")
    );
    // Unknown file; file that was never completed.
    assert_eq!(
      s.app
        .post(&s.carol, "/shares", json!({"attachment_id": "1"}))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    let announced = s
      .app
      .post(
        &s.carol,
        "/attachments",
        json!({"conversation_id": s.channel, "size_bytes": 10}),
      )
      .await;
    let pending = announced.body["id"].as_str().unwrap();
    assert_eq!(
      s.app
        .post(&s.carol, "/shares", json!({"attachment_id": pending}))
        .await
        .status,
      StatusCode::CONFLICT
    );
    // Each person lists only their own links.
    assert_eq!(s.app.get(&s.carol, "/shares").await.body["links"], json!([]));
    assert_eq!(
      s.app.get(&s.dave, "/shares").await.body["links"]
        .as_array()
        .unwrap()
        .len(),
      1
    );
    s.app.stop().await;
  }

  #[tokio::test]
  async fn guests_do_not_get_the_permission_by_default_and_admins_can_grant_it() {
    let Some(app) = TestApp::start_with_storage(|_| {}).await else {
      return;
    };
    let (alice, bob, ext) = (owner(), admin(), guest("ext"));
    let ext_id = user_id(&app, &ext).await;
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    add_space_member(&app, &alice, &space, &ext_id, "guest").await;
    let channel = create_channel(&app, &alice, &space, "shared", "public").await;
    add_channel_member(&app, &alice, &channel, &ext_id).await;
    let file = upload(&app, &ext, &channel, BLOB).await;
    let denied = app.post(&ext, "/shares", json!({"attachment_id": file})).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{}", denied.body);

    // An instance admin switches it on for the guest role (channel content permissions are fine).
    let changed = app
      .put(
        &bob,
        "/admin/permission-scheme",
        json!({"changes": [{"role": "guest", "permission": "create_share_link", "effect": "grant"}]}),
      )
      .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
    assert_eq!(
      app
        .post(&ext, "/shares", json!({"attachment_id": file}))
        .await
        .status,
      StatusCode::CREATED
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn the_feature_can_be_switched_off() {
    let Some(s) = scene_with(|settings| settings.shares.is_enabled = false).await else {
      return;
    };
    assert_eq!(
      share(&s, json!({"attachment_id": s.attachment})).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      public(&s.app, reqwest::Method::GET, "/shares/anything").await.0,
      StatusCode::GONE
    );
    s.app.stop().await;
  }

  #[tokio::test]
  async fn deleting_the_attachment_ends_its_links() {
    let Some(s) = scene().await else { return };
    let id = share(&s, json!({"attachment_id": s.attachment})).await.body["id"]
      .as_str()
      .unwrap()
      .to_owned();
    sqlx::query("DELETE FROM attachments")
      .execute(s.app.sql_pool())
      .await
      .unwrap();
    assert_eq!(
      public(&s.app, reqwest::Method::GET, &format!("/shares/{id}"))
        .await
        .0,
      StatusCode::GONE
    );
    assert_eq!(s.app.get(&s.carol, "/shares").await.body["links"], json!([]));
    s.app.stop().await;
  }
}

mod public_endpoints {
  use super::*;

  #[tokio::test]
  async fn the_public_endpoints_are_rate_limited_per_address() {
    let Some(s) = scene_with(|settings| {
      settings.security.rate_limits.is_enabled = true;
      settings.security.rate_limits.public_requests_per_minute = 5;
    })
    .await
    else {
      return;
    };
    let mut statuses = Vec::new();
    for _ in 0..12 {
      statuses.push(public(&s.app, reqwest::Method::GET, "/shares/guess").await.0);
    }
    assert!(statuses.contains(&StatusCode::TOO_MANY_REQUESTS), "{statuses:?}");
    s.app.stop().await;
  }
}

mod page {
  use super::*;

  struct Bundle(PathBuf);

  impl Bundle {
    fn create(html: &str) -> Self {
      let dir = std::env::temp_dir().join(format!(
        "teamcraft-share-page-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .unwrap()
          .as_nanos()
      ));
      std::fs::create_dir_all(&dir).unwrap();
      std::fs::write(dir.join("download.html"), html).unwrap();
      std::fs::write(dir.join("download.js"), b"console.log('download page');").unwrap();
      std::fs::write(dir.join("download.css"), b"body{margin:0}").unwrap();
      Self(dir)
    }
  }

  impl Drop for Bundle {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  const PAGE: &str = "<!doctype html><link rel=stylesheet href=\"/f-assets/download.css\" integrity=\"{{sri:download.css}}\">\
    <script src=\"/f-assets/download.js\" integrity=\"{{sri:download.js}}\"></script>";

  #[tokio::test]
  async fn the_download_page_carries_integrity_values_and_strict_headers() {
    let bundle = Bundle::create(PAGE);
    let dir = bundle.0.clone();
    let Some(app) = TestApp::start_with(move |settings| settings.web.maybe_share_page_dir = Some(dir)).await
    else {
      return;
    };
    let response = app
      .http
      .get(format!("{}/f/some-share-id", app.base_url))
      .send()
      .await
      .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    let html = response.text().await.unwrap();
    let expect = teamcraft_crypto::account::subresource_integrity;
    assert!(html.contains(&format!("integrity=\"{}\"", expect(b"body{margin:0}"))));
    assert!(!html.contains("{{sri:"));
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    let csp = headers["content-security-policy"].to_str().unwrap();
    assert!(
      csp.starts_with("default-src 'none'") && csp.contains("frame-ancestors 'none'"),
      "{csp}"
    );

    // The assets are served from /f-assets and must be revalidated; the template itself is not public
    // in a way that matters, the served page is what counts.
    let script = app
      .http
      .get(format!("{}/f-assets/download.js", app.base_url))
      .send()
      .await
      .unwrap();
    assert_eq!(script.status(), StatusCode::OK);
    assert_eq!(script.headers()["cache-control"], "no-cache");
    assert_eq!(script.text().await.unwrap(), "console.log('download page');");
    assert_eq!(
      app
        .http
        .get(format!("{}/f-assets/missing.js", app.base_url))
        .send()
        .await
        .unwrap()
        .status(),
      StatusCode::NOT_FOUND
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_bundle_that_cannot_be_checked_is_not_served() {
    for html in [
      "<script integrity=\"{{sri:missing.js}}\"></script>",
      "<script integrity=\"{{sri:../outside.js}}\"></script>",
      "<script integrity=\"{{sri:/etc/passwd}}\"></script>",
      "<script integrity=\"{{sri:download.js\"></script>",
    ] {
      let bundle = Bundle::create(html);
      let dir = bundle.0.clone();
      let Some(app) =
        TestApp::start_with(move |settings| settings.web.maybe_share_page_dir = Some(dir)).await
      else {
        return;
      };
      let response = app
        .http
        .get(format!("{}/f/x", app.base_url))
        .send()
        .await
        .unwrap();
      assert_eq!(response.status(), StatusCode::NOT_FOUND, "{html}");
      app.stop().await;
    }
  }
}
