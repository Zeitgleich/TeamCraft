//! Profiles and avatars against a real server, Postgres and the dev Garage.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::*;
use image::ImageFormat;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::GatewayEvent;
use teamcraft_server::testing::{TestApp, TestUser};

use crate::session_policy::publish_policy;
use crate::space_images::{noise_png, picture, put_image};

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
      json!({"time_zone": "Mars/Base"}),
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

  #[tokio::test]
  async fn an_upload_is_re_encoded_as_a_square_png_and_downloaded_by_people_who_see_the_user() {
    let app = app!(|settings| settings.limits.avatar_side_px = 128);
    let (carol, dave) = (member("carol"), member("dave"));
    let carol_id = user_id(&app, &carol).await;
    channel_with(&app, &[&carol, &dave]).await;

    assert_eq!(
      app.get(&dave, &format!("/users/{carol_id}/avatar")).await.status,
      StatusCode::NOT_FOUND
    );
    // A wide JPEG: the server keeps the middle square and scales it to the configured side.
    let (status, profile) = put_image(
      &app,
      &carol,
      "/me/avatar",
      "image/jpeg",
      &picture(600, 300, ImageFormat::Jpeg),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{profile}");
    assert_eq!(profile["has_avatar"], true);
    assert_eq!(profile["can_change_avatar"], true);
    assert!(
      profile["avatar_version"]
        .as_str()
        .is_some_and(|version| !version.contains('/')),
      "clients key their cached copy by an opaque version"
    );

    let png = download(&app, &dave, &carol_id).await;
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "re-encoded as PNG");
    let decoded = image::load_from_memory(&png).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (128, 128));

    // A second picture replaces the first under a new version.
    let (_, second) = put_image(
      &app,
      &carol,
      "/me/avatar",
      "image/png",
      &picture(64, 64, ImageFormat::Png),
    )
    .await;
    assert_ne!(second["avatar_version"], profile["avatar_version"]);
    let decoded = image::load_from_memory(&download(&app, &dave, &carol_id).await).unwrap();
    assert_eq!(
      (decoded.width(), decoded.height()),
      (64, 64),
      "small pictures stay small"
    );

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
  async fn only_raster_pictures_of_the_declared_type_within_the_limit_are_taken() {
    let app = app!(|settings| settings.limits.max_avatar_bytes = 4000);
    let carol = member("carol");
    let png = picture(8, 8, ImageFormat::Png);
    for (content_type, body, status) in [
      ("image/png", Vec::new(), StatusCode::BAD_REQUEST),
      ("image/jpeg", png.clone(), StatusCode::BAD_REQUEST),
      ("image/gif", b"GIF89a....".to_vec(), StatusCode::BAD_REQUEST),
      (
        "image/svg+xml",
        br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#.to_vec(),
        StatusCode::BAD_REQUEST,
      ),
      ("text/html", b"<html></html>".to_vec(), StatusCode::BAD_REQUEST),
      (
        "image/png",
        png[..png.len() / 2].to_vec(),
        StatusCode::BAD_REQUEST,
      ),
      ("image/png", noise_png(200), StatusCode::PAYLOAD_TOO_LARGE),
    ] {
      let (answer, body) = put_image(&app, &carol, "/me/avatar", content_type, &body).await;
      assert_eq!(answer, status, "{content_type}: {body}");
    }
    assert_eq!(app.get(&carol, "/me/profile").await.body["has_avatar"], false);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_picture_from_the_identity_provider_shows_and_locks_the_own_one_unless_overrides_are_allowed() {
    let app = app!();
    let (carol, dave) = (member("carol"), member("dave"));
    let carol_id = user_id(&app, &carol).await;
    channel_with(&app, &[&carol, &dave]).await;
    let (_, own) = put_image(
      &app,
      &carol,
      "/me/avatar",
      "image/png",
      &picture(32, 32, ImageFormat::Png),
    )
    .await;

    publish_policy(&app, 1, |p| {
      p["directory"]["avatar_source"] = json!("identity_provider")
    })
    .await;
    // No provider picture yet: the own one still shows, and may still be changed.
    let profile = app.get(&carol, "/me/profile").await.body;
    assert_eq!(profile["avatar_version"], own["avatar_version"]);
    assert_eq!(profile["can_change_avatar"], true);

    // The provider sends a picture through SCIM; it is imported in the background.
    let scim = scim_token(&app).await;
    let photo = format!(
      "data:image/jpeg;base64,{}",
      STANDARD.encode(picture(300, 200, ImageFormat::Jpeg))
    );
    let created = scim_send(
      &app,
      &scim,
      reqwest::Method::POST,
      "/Users",
      &json!({
        "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
        "userName": carol.subject,
        "active": true,
        "photos": [{"value": photo, "type": "photo", "primary": true}],
      }),
    )
    .await;
    assert_eq!(created.0, StatusCode::CREATED, "{}", created.1);
    let scim_id = created.1["id"].as_str().unwrap().to_owned();
    let profile = profile_until(&app, &carol, |profile| {
      profile["avatar_version"] != own["avatar_version"]
    })
    .await;
    assert_eq!(profile["can_change_avatar"], false);
    let decoded = image::load_from_memory(&download(&app, &dave, &carol_id).await).unwrap();
    assert_eq!(
      (decoded.width(), decoded.height()),
      (200, 200),
      "the middle square"
    );

    let (status, refused) = put_image(
      &app,
      &carol,
      "/me/avatar",
      "image/png",
      &picture(16, 16, ImageFormat::Png),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error_code"], "avatar_from_identity_provider");
    assert_eq!(
      app.delete(&carol, "/me/avatar").await.status,
      StatusCode::FORBIDDEN
    );

    // With overrides allowed, the own picture shows again and may be changed.
    publish_policy(&app, 2, |p| {
      p["directory"]["avatar_source"] = json!("identity_provider");
      p["directory"]["allow_avatar_override"] = json!(true);
    })
    .await;
    let profile = app.get(&carol, "/me/profile").await.body;
    assert_eq!(profile["avatar_version"], own["avatar_version"]);
    assert_eq!(profile["can_change_avatar"], true);
    publish_policy(&app, 3, |p| {
      p["directory"]["avatar_source"] = json!("identity_provider")
    })
    .await;

    // A replace without photos removes the provider's picture: the own one shows again.
    let replaced = scim_send(
      &app,
      &scim,
      reqwest::Method::PUT,
      &format!("/Users/{scim_id}"),
      &json!({
        "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
        "userName": carol.subject,
        "active": true,
      }),
    )
    .await;
    assert_eq!(replaced.0, StatusCode::OK, "{}", replaced.1);
    let profile = app.get(&carol, "/me/profile").await.body;
    assert_eq!(profile["avatar_version"], own["avatar_version"]);
    assert_eq!(profile["can_change_avatar"], true);
    app.stop().await;
  }

  async fn download(app: &TestApp, viewer: &TestUser, user: &str) -> Vec<u8> {
    let avatar = app.get(viewer, &format!("/users/{user}/avatar")).await;
    assert_eq!(avatar.status, StatusCode::OK, "{}", avatar.body);
    assert_eq!(avatar.body["content_type"], "image/png");
    let fetched = app
      .http
      .get(avatar.body["download_url"].as_str().unwrap())
      .send()
      .await
      .unwrap();
    assert_eq!(fetched.status(), StatusCode::OK);
    let bytes = fetched.bytes().await.unwrap().to_vec();
    assert_eq!(avatar.body["size_bytes"], bytes.len());
    bytes
  }

  /// The caller's profile once `is_done` holds; the import runs in the background.
  async fn profile_until(app: &TestApp, user: &TestUser, is_done: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
      let profile = app.get(user, "/me/profile").await.body;
      if is_done(&profile) {
        return profile;
      }
      assert!(tokio::time::Instant::now() < deadline, "not imported: {profile}");
      tokio::time::sleep(Duration::from_millis(50)).await;
    }
  }

  pub(super) async fn scim_token(app: &TestApp) -> String {
    user_id(app, &admin()).await;
    let reply = app
      .post(
        &admin(),
        "/admin/scim/clients",
        json!({"name": "Identity provider", "identity_attribute": "user_name"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    reply.body["token"].as_str().unwrap().to_owned()
  }

  pub(super) async fn scim_send(
    app: &TestApp,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: &Value,
  ) -> (StatusCode, Value) {
    let response = app
      .http
      .request(method, format!("{}/scim/v2{path}", app.base_url))
      .bearer_auth(token)
      .header("content-type", "application/scim+json")
      .body(body.to_string())
      .send()
      .await
      .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
  }
}

/// Imports of provider pictures under races, a bulk sync and two sources (issue #2125), steered
/// through the import probe instead of sleeping.
mod provider_imports {
  use std::str::FromStr;
  use std::sync::atomic::Ordering;

  use teamcraft_proto::UserId;
  use teamcraft_server::services::provider_avatars::{ImportProbe, ProviderPictures};

  use super::avatars::{scim_send, scim_token};
  use super::*;

  const CLAIM: &str = "https://pictures.example.com/claim.png";

  #[tokio::test]
  async fn a_removal_during_an_import_wins_and_the_newest_photo_is_imported_next() {
    let app = app!();
    identity_provider_pictures(&app).await;
    let scim = scim_token(&app).await;
    let probe = app.state.provider_pictures.probe();

    // The provider removes the photo while its import runs: nothing comes back.
    probe.is_pausing.store(true, Ordering::SeqCst);
    let user = scim_user(&app, &scim, "frank", Some(&photo(300, 200))).await;
    paused(probe).await;
    put_user(&app, &scim, &user, "frank", None).await;
    probe.resume.add_permits(1);
    finished(probe, 1).await;
    assert_eq!(probe.superseded.load(Ordering::SeqCst), 1);
    assert_eq!(stored_source(&app, &user).await, None);

    // Two photos while an import runs: the running one is dropped, the newest is imported.
    put_user(&app, &scim, &user, "frank", Some(&photo(300, 200))).await;
    paused(probe).await;
    let newest = photo(120, 90);
    put_user(&app, &scim, &user, "frank", Some(&newest)).await;
    probe.is_pausing.store(false, Ordering::SeqCst);
    probe.resume.add_permits(1);
    finished(probe, 2).await;
    assert_eq!(probe.superseded.load(Ordering::SeqCst), 2);
    assert_eq!(probe.stored.load(Ordering::SeqCst), 1);
    assert_eq!(stored_source(&app, &user).await, Some(sri(&newest)));
    app.stop().await;
  }

  #[tokio::test]
  async fn a_removal_while_the_photo_waits_for_a_free_import_wins() {
    let app = app!(|settings| settings.limits.max_concurrent_avatar_imports = 1);
    identity_provider_pictures(&app).await;
    let scim = scim_token(&app).await;
    let probe = app.state.provider_pictures.probe();
    // Someone else's import holds the only permit.
    probe.is_pausing.store(true, Ordering::SeqCst);
    let busy = scim_user(&app, &scim, "ivan", Some(&photo(300, 200))).await;
    paused(probe).await;
    take(&probe.spawned, 1).await;
    let user = scim_user(&app, &scim, "judy", None).await;

    // The photo's write waits for the permit; the removal comes meanwhile and completes.
    let removed_photo = photo(120, 90);
    let set = put_user(&app, &scim, &user, "judy", Some(&removed_photo));
    let remove_meanwhile = async {
      take(&probe.spawned, 1).await;
      put_user(&app, &scim, &user, "judy", None).await;
      probe.is_pausing.store(false, Ordering::SeqCst);
      probe.resume.add_permits(1);
    };
    tokio::join!(set, remove_meanwhile);
    finished(probe, 2).await;
    assert_eq!(probe.superseded.load(Ordering::SeqCst), 1);
    assert_eq!(
      stored_source(&app, &user).await,
      None,
      "the photo doesn't come back"
    );
    assert!(stored_source(&app, &busy).await.is_some());
    app.stop().await;
  }

  #[tokio::test]
  async fn a_bulk_sync_imports_every_photo_with_no_more_imports_at_once_than_the_cap() {
    const PEOPLE: usize = 8;
    let app = app!(|settings| {
      settings.limits.max_concurrent_avatar_imports = 2;
      settings.limits.max_concurrent_image_decodes = 1;
    });
    identity_provider_pictures(&app).await;
    let scim = scim_token(&app).await;
    let probe = app.state.provider_pictures.probe();
    let picture = photo(300, 200);
    let names: Vec<String> = (0..PEOPLE).map(|n| format!("bulk-{n}")).collect();
    let people = futures_util::future::join_all(
      names
        .iter()
        .map(|name| scim_user(&app, &scim, name, Some(&picture))),
    )
    .await;
    finished(probe, PEOPLE).await;
    assert_eq!(probe.failed.load(Ordering::SeqCst), 0, "none failed as busy");
    assert_eq!(probe.stored.load(Ordering::SeqCst), PEOPLE);
    assert!(probe.most_at_once.load(Ordering::SeqCst) <= 2);
    for user in &people {
      assert_eq!(stored_source(&app, user).await, Some(sri(&picture)));
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn a_scim_photo_and_a_differing_claim_dont_take_turns() {
    let app = app!();
    identity_provider_pictures(&app).await;
    let scim = scim_token(&app).await;
    let probe = app.state.provider_pictures.probe();
    let picture = photo(300, 200);
    let user = scim_user(&app, &scim, "grace", Some(&picture)).await;
    finished(probe, 1).await;
    for _ in 0..3 {
      ProviderPictures::note_sign_in(&app.state, user.id, Some(CLAIM));
      put_user(&app, &scim, &user, "grace", Some(&picture)).await;
    }
    assert_eq!(
      probe.started.load(Ordering::SeqCst),
      1,
      "only the SCIM photo was imported"
    );
    assert_eq!(stored_source(&app, &user).await, Some(sri(&picture)));
    app.stop().await;
  }

  #[tokio::test]
  async fn a_claim_of_a_person_scim_manages_is_left_to_scim_without_a_remembered_source() {
    let app = app!();
    let scim = scim_token(&app).await;
    // Written while people chose their own pictures: this run remembers nothing of them.
    let user = scim_user(&app, &scim, "heidi", None).await;
    identity_provider_pictures(&app).await;
    let probe = app.state.provider_pictures.probe();
    ProviderPictures::note_sign_in(&app.state, user.id, Some(CLAIM));
    finished(probe, 1).await;
    assert_eq!(probe.left_to_scim.load(Ordering::SeqCst), 1);
    assert_eq!(stored_source(&app, &user).await, None);
    // Remembered from now on: the next claim costs a map lookup, no worker.
    ProviderPictures::note_sign_in(&app.state, user.id, Some(CLAIM));
    assert_eq!(probe.spawned.available_permits(), 1);
    assert_eq!(probe.started.load(Ordering::SeqCst), 0);
    app.stop().await;
  }

  /// A user a SCIM client wrote.
  struct ScimUser {
    id: UserId,
  }

  async fn identity_provider_pictures(app: &TestApp) {
    publish_policy(app, 1, |p| {
      p["directory"]["avatar_source"] = json!("identity_provider")
    })
    .await;
  }

  async fn scim_user(app: &TestApp, scim: &str, name: &str, maybe_photo: Option<&str>) -> ScimUser {
    let (status, body) = scim_send(
      app,
      scim,
      reqwest::Method::POST,
      "/Users",
      &user_resource(name, maybe_photo),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    ScimUser {
      id: UserId::from_str(body["id"].as_str().unwrap()).unwrap(),
    }
  }

  async fn put_user(app: &TestApp, scim: &str, user: &ScimUser, name: &str, maybe_photo: Option<&str>) {
    let (status, body) = scim_send(
      app,
      scim,
      reqwest::Method::PUT,
      &format!("/Users/{}", user.id),
      &user_resource(name, maybe_photo),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
  }

  fn user_resource(name: &str, maybe_photo: Option<&str>) -> Value {
    let mut resource = json!({
      "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
      "userName": name,
      "active": true,
    });
    if let Some(photo) = maybe_photo {
      resource["photos"] = json!([{"value": photo, "type": "photo", "primary": true}]);
    }
    resource
  }

  fn photo(width: u32, height: u32) -> String {
    format!(
      "data:image/jpeg;base64,{}",
      STANDARD.encode(picture(width, height, ImageFormat::Jpeg))
    )
  }

  /// What the server keeps to recognize an inline photo.
  fn sri(data_uri: &str) -> String {
    teamcraft_crypto::account::subresource_integrity(data_uri.as_bytes())
  }

  async fn stored_source(app: &TestApp, user: &ScimUser) -> Option<String> {
    app.state.database.provider_picture_source(user.id).await.unwrap()
  }

  async fn paused(probe: &ImportProbe) {
    tokio::time::timeout(Duration::from_secs(30), probe.paused.acquire())
      .await
      .expect("an import pauses before storing")
      .unwrap()
      .forget();
  }

  async fn finished(probe: &ImportProbe, count: usize) {
    take(&probe.finished, count).await;
  }

  async fn take(permits: &tokio::sync::Semaphore, count: usize) {
    tokio::time::timeout(
      Duration::from_secs(60),
      permits.acquire_many(u32::try_from(count).unwrap()),
    )
    .await
    .expect("the probe is told in time")
    .unwrap()
    .forget();
  }
}
