//! Runtime settings of the admin console: brands, the mail server, the calendar and the instance
//! settings (admin.md §6). Every endpoint is for instance admins only and leaves an audit entry.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use crate::common;

use std::sync::Arc;

use common::*;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use teamcraft_server::config::{BrandConfig, MailTexts, MeetingsConfig, UnknownHostPolicy};
use teamcraft_server::services::brands;
use teamcraft_server::testing::{FakeCalendar, FakeMailer, TestApp, TestUser};

const HOST_A: &str = "chat.a.example.com";
const HOST_C: &str = "chat.c.example.com";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";
const CALCRAFT_URL: &str = "http://127.0.0.1:5232/dav/calendars/teamcraft/default/";
const MAILPIT_PORT: u16 = 1025;

macro_rules! app {
  ($start:expr) => {
    match $start.await {
      Some(app) => app,
      None => return,
    }
  };
}

/// One configured brand `a` on HOST_A and HOST_C as an extra served host name.
async fn branded() -> Option<TestApp> {
  TestApp::start_with(|settings| {
    settings.brands = vec![BrandConfig {
      id: "a".into(),
      is_default: true,
      hostnames: vec![HOST_A.into()],
      display_name: "Brand A".into(),
      ..BrandConfig::default()
    }];
    settings.branding.served_hostnames = vec![HOST_C.into()];
    // The test client calls by address; brand hosts are named per request.
    settings.branding.unknown_host = UnknownHostPolicy::Default;
  })
  .await
}

fn meetings_config() -> MeetingsConfig {
  MeetingsConfig {
    public_base_url: "https://chat.example.com".into(),
    organizer_fallback_address: "meetings@example.com".into(),
    caldav: None,
    max_invitees: 10,
    guest_token_secret: "0123456789abcdef0123456789abcdef".to_owned().into(),
    guest_grace_secs: 7200,
    maybe_inbox_token: None,
    smtp: None,
    mail_texts: MailTexts::default(),
  }
}

fn brand_settings(name: &str, hosts: &[&str]) -> Value {
  json!({
    "display_name": name,
    "organization_name": format!("{name} Ltd."),
    "hostnames": hosts,
    "email_domains": ["c.example.com"],
    "colors": {"light": {"accent": "#0b7f78", "on_accent": "#ffffff"}, "dark": {"accent": "#35c6ba"}},
    "login_title": {"en": "Welcome", "de": "Willkommen"},
    "login_text": {"en": "Sign in with your company account."},
    "links": {"support": "https://support.c.example.com"},
    "mail": {"sender_name": format!("{name} Team"), "from_address": "chat@c.example.com"},
  })
}

fn find_brand<'a>(body: &'a Value, id: &str) -> &'a Value {
  body["brands"]
    .as_array()
    .unwrap()
    .iter()
    .find(|brand| brand["id"] == id)
    .unwrap_or_else(|| panic!("no brand {id} in {body}"))
}

async fn public_brand(app: &TestApp, host: &str) -> (StatusCode, Value) {
  let response = app
    .http
    .get(format!("{}/api/v1/brand", app.base_url))
    .header("Host", host)
    .send()
    .await
    .unwrap();
  let status = response.status();
  (status, response.json().await.unwrap_or(Value::Null))
}

async fn audit_actions(app: &TestApp, admin: &TestUser) -> Vec<String> {
  app.get(admin, "/admin/audit?limit=200").await.body["entries"]
    .as_array()
    .unwrap()
    .iter()
    .map(|entry| entry["action"].as_str().unwrap().to_owned())
    .collect()
}

async fn put_bytes(
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

fn smtp_request(host: &str, port: u16, password: Value) -> Value {
  json!({
    "host": host,
    "port": port,
    "security": "none",
    "username": "teamcraft",
    "password": password,
    "from_address": "no-reply@example.com",
    "from_name": "TeamCraft",
  })
}

async fn is_reachable(address: &str) -> bool {
  tokio::time::timeout(
    std::time::Duration::from_secs(1),
    tokio::net::TcpStream::connect(address),
  )
  .await
  .is_ok_and(|result| result.is_ok())
}

mod permissions {
  use super::*;

  #[tokio::test]
  async fn only_instance_admins_reach_the_settings_endpoints() {
    let app = app!(branded());
    let (bob, carol) = (admin(), member("carol"));
    let endpoints: Vec<(Method, &str, Option<Value>)> = vec![
      (Method::GET, "/admin/brands", None),
      (
        Method::POST,
        "/admin/brands",
        Some(json!({"id": "c", "settings": brand_settings("C", &[HOST_C])})),
      ),
      (
        Method::PUT,
        "/admin/brands/a",
        Some(json!({"settings": brand_settings("A", &[HOST_A])})),
      ),
      (Method::DELETE, "/admin/brands/a", None),
      (Method::POST, "/admin/brands/a/take-over", None),
      (Method::GET, "/admin/brands/a/assets/logo", None),
      (Method::DELETE, "/admin/brands/a/assets/logo", None),
      (Method::GET, "/admin/settings/mail", None),
      (
        Method::PUT,
        "/admin/settings/mail",
        Some(smtp_request("localhost", 25, json!({"action": "keep"}))),
      ),
      (Method::DELETE, "/admin/settings/mail", None),
      (Method::POST, "/admin/settings/mail/test", Some(json!({}))),
      (Method::GET, "/admin/settings/calendar", None),
      (
        Method::PUT,
        "/admin/settings/calendar",
        Some(
          json!({"calendar_url": "https://cal.example.com/", "username": "u", "password": {"action": "keep"}}),
        ),
      ),
      (Method::DELETE, "/admin/settings/calendar", None),
      (Method::POST, "/admin/settings/calendar/test", None),
      (Method::GET, "/admin/settings", None),
      (
        Method::PUT,
        "/admin/settings",
        Some(json!({"section": "integration", "values": {"default_retention_days": 7}})),
      ),
      (Method::DELETE, "/admin/settings/integration", None),
    ];
    for (method, path, body) in endpoints {
      let as_member = app.call(method.clone(), Some(&carol), path, body.clone()).await;
      assert_eq!(
        as_member.status,
        StatusCode::FORBIDDEN,
        "{method} {path} as member"
      );
      let anonymous = app.call(method.clone(), None, path, body).await;
      assert_eq!(
        anonymous.status,
        StatusCode::UNAUTHORIZED,
        "{method} {path} without token"
      );
    }
    let (status, _) = put_bytes(&app, &carol, "/admin/brands/a/assets/logo", "image/png", PNG).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Admins do reach them.
    assert_eq!(app.get(&bob, "/admin/brands").await.status, StatusCode::OK);
    assert!(
      audit_actions(&app, &bob)
        .await
        .iter()
        .all(|action| !action.starts_with("brand_"))
    );
    app.stop().await;
  }
}

mod brand_settings {
  use super::*;

  #[tokio::test]
  async fn configured_brands_are_read_only_until_taken_over_and_come_back_when_deleted() {
    let app = app!(branded());
    let bob = admin();
    let listed = app.get(&bob, "/admin/brands").await;
    assert_eq!(listed.status, StatusCode::OK);
    let a = find_brand(&listed.body, "a");
    assert_eq!(a["source"], "config");
    assert_eq!(a["is_default"], true);
    let served = listed.body["served_hostnames"].as_array().unwrap();
    assert!(
      served.contains(&json!({"hostname": HOST_A, "brand_id": "a"})),
      "{served:?}"
    );
    assert!(served.contains(&json!({"hostname": HOST_C})), "{served:?}");

    let edit = json!({"is_default": true, "settings": brand_settings("Brand A2", &[HOST_A])});
    let refused = app.put(&bob, "/admin/brands/a", edit.clone()).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.error_code(), "conflict");

    let taken = app.post(&bob, "/admin/brands/a/take-over", json!({})).await;
    assert_eq!(taken.status, StatusCode::OK, "{}", taken.body);
    let a = find_brand(&taken.body, "a");
    assert_eq!(a["source"], "database");
    assert_eq!(a["is_overriding_config"], true);

    let edited = app.put(&bob, "/admin/brands/a", edit).await;
    assert_eq!(edited.status, StatusCode::OK, "{}", edited.body);
    let (_, public) = public_brand(&app, HOST_A).await;
    assert_eq!(public["display_name"], "Brand A2");
    assert_eq!(public["organization_name"], "Brand A2 Ltd.");
    assert_eq!(public["login_title"]["de"], "Willkommen");

    let deleted = app.delete(&bob, "/admin/brands/a").await;
    assert_eq!(deleted.status, StatusCode::OK);
    assert_eq!(find_brand(&deleted.body, "a")["source"], "config");
    let (_, public) = public_brand(&app, HOST_A).await;
    assert_eq!(public["display_name"], "Brand A");

    let actions = audit_actions(&app, &bob).await;
    for action in ["brand_taken_over", "brand_updated", "brand_deleted"] {
      assert!(
        actions.iter().any(|a| a == action),
        "{action} missing in {actions:?}"
      );
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn new_brands_use_served_host_names_only_and_survive_a_restart() {
    let app = app!(branded());
    let bob = admin();
    let unserved = app
      .post(
        &bob,
        "/admin/brands",
        json!({"id": "c", "settings": brand_settings("C", &["evil.example.com"])}),
      )
      .await;
    assert_eq!(unserved.status, StatusCode::BAD_REQUEST);
    assert!(unserved.body["message"].as_str().unwrap().contains("not served"));
    let stolen = app
      .post(
        &bob,
        "/admin/brands",
        json!({"id": "c", "settings": brand_settings("C", &[HOST_A])}),
      )
      .await;
    assert_eq!(
      stolen.status,
      StatusCode::BAD_REQUEST,
      "a host name of another brand"
    );
    let bad_color = {
      let mut settings = brand_settings("C", &[HOST_C]);
      settings["colors"]["light"]["accent"] = json!("red;}");
      app
        .post(&bob, "/admin/brands", json!({"id": "c", "settings": settings}))
        .await
    };
    assert_eq!(bad_color.status, StatusCode::BAD_REQUEST);
    let bad_id = app
      .post(
        &bob,
        "/admin/brands",
        json!({"id": "Brand C", "settings": brand_settings("C", &[HOST_C])}),
      )
      .await;
    assert_eq!(bad_id.status, StatusCode::BAD_REQUEST);
    let taken_id = app
      .post(
        &bob,
        "/admin/brands",
        json!({"id": "a", "settings": brand_settings("C", &[HOST_C])}),
      )
      .await;
    assert_eq!(taken_id.status, StatusCode::CONFLICT);

    let created = app
      .post(
        &bob,
        "/admin/brands",
        json!({"id": "c", "settings": brand_settings("Brand C", &[HOST_C])}),
      )
      .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    let served = created.body["served_hostnames"].as_array().unwrap();
    assert!(served.contains(&json!({"hostname": HOST_C, "brand_id": "c"})));
    let (status, public) = public_brand(&app, HOST_C).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(public["display_name"], "Brand C");
    assert_eq!(public["origin"], format!("https://{HOST_C}"));
    // The received host never chooses anything else.
    let (_, other) = public_brand(&app, HOST_A).await;
    assert_eq!(other["display_name"], "Brand A");

    // A restart reads the brand back from the database.
    let config_only = app.state.brands.check(&[]).unwrap();
    app.state.brands.install(config_only, Vec::new());
    assert_eq!(public_brand(&app, HOST_C).await.1["display_name"], "Brand A");
    brands::load(&app.state).await.unwrap();
    assert_eq!(public_brand(&app, HOST_C).await.1["display_name"], "Brand C");
    assert!(
      audit_actions(&app, &bob)
        .await
        .contains(&"brand_created".to_owned())
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_new_default_takes_over_from_the_configured_one() {
    let app = app!(branded());
    let bob = admin();
    let created = app
      .post(
        &bob,
        "/admin/brands",
        json!({"id": "c", "is_default": true, "settings": brand_settings("Brand C", &[HOST_C])}),
      )
      .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    assert_eq!(created.body["brands"][0]["id"], "c", "the default comes first");
    assert_eq!(find_brand(&created.body, "a")["is_default"], false);
    assert_eq!(app.state.brands.current().default_brand().id, "c");
    app.stop().await;
  }

  #[tokio::test]
  async fn images_need_the_object_storage_and_a_matching_type() {
    let app = app!(branded());
    let bob = admin();
    app
      .post(
        &bob,
        "/admin/brands",
        json!({"id": "c", "settings": brand_settings("Brand C", &[HOST_C])}),
      )
      .await;
    let (status, body) = put_bytes(&app, &bob, "/admin/brands/c/assets/logo", "image/png", PNG).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let (status, _) = put_bytes(&app, &bob, "/admin/brands/a/assets/logo", "image/png", PNG).await;
    assert_eq!(status, StatusCode::CONFLICT, "configuration brands are read-only");
    assert_eq!(
      app.get(&bob, "/admin/brands/c/assets/logo").await.status,
      StatusCode::NOT_FOUND
    );
    assert_eq!(
      app.get(&bob, "/admin/brands/x/assets/logo").await.error_code(),
      "brand_not_found"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn images_are_stored_served_on_the_brand_host_and_removed() {
    let app = app!(TestApp::start_with_storage(|settings| {
      settings.branding.served_hostnames = vec![HOST_C.into()];
      settings.branding.unknown_host = UnknownHostPolicy::Default;
    }));
    let bob = admin();
    let created = app
      .post(
        &bob,
        "/admin/brands",
        json!({"id": "c", "settings": brand_settings("Brand C", &[HOST_C])}),
      )
      .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    assert_eq!(created.body["is_image_upload_available"], true);

    let (status, _) = put_bytes(&app, &bob, "/admin/brands/c/assets/logo", "image/jpeg", PNG).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "declared type must match");
    let (status, _) = put_bytes(&app, &bob, "/admin/brands/c/assets/logo", "text/html", b"<html>").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = put_bytes(&app, &bob, "/admin/brands/c/assets/hero", "image/png", PNG).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown slot");
    let too_big = [PNG, &vec![0u8; 600 * 1024]].concat();
    let (status, _) = put_bytes(&app, &bob, "/admin/brands/c/assets/logo", "image/png", &too_big).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    let (status, body) = put_bytes(&app, &bob, "/admin/brands/c/assets/logo", "image/png", PNG).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(find_brand(&body, "c")["assets"], json!(["logo"]));
    let public = app
      .http
      .get(format!("{}/api/v1/brand/assets/logo", app.base_url))
      .header("Host", HOST_C)
      .send()
      .await
      .unwrap();
    assert_eq!(public.status(), StatusCode::OK);
    assert_eq!(public.headers()["content-type"], "image/png");
    assert_eq!(public.bytes().await.unwrap().as_ref(), PNG);
    let preview = app
      .http
      .get(format!("{}/api/v1/admin/brands/c/assets/logo", app.base_url))
      .bearer_auth(app.token(&bob))
      .send()
      .await
      .unwrap();
    assert_eq!(preview.headers()["cache-control"], "private, no-store");

    // A restart reads the image back from the storage.
    let config_only = app.state.brands.check(&[]).unwrap();
    app.state.brands.install(config_only, Vec::new());
    brands::load(&app.state).await.unwrap();
    let brand = app.state.brands.current();
    assert!(
      brand
        .by_id("c")
        .unwrap()
        .asset(&teamcraft_proto::BrandAssetSlot::Logo)
        .is_some()
    );

    let removed = app.delete(&bob, "/admin/brands/c/assets/logo").await;
    assert_eq!(removed.status, StatusCode::OK);
    assert_eq!(find_brand(&removed.body, "c")["assets"], json!([]));
    let actions = audit_actions(&app, &bob).await;
    assert!(
      actions.contains(&"brand_asset_set".to_owned()) && actions.contains(&"brand_asset_removed".to_owned())
    );
    app.stop().await;
  }
}

mod mail_settings {
  use super::*;

  #[tokio::test]
  async fn the_password_is_write_only_and_sealed_in_the_database() {
    let app = app!(TestApp::start_with_mailer(
      |_| {},
      Arc::new(FakeMailer::default())
    ));
    let bob = admin();
    let initial = app.get(&bob, "/admin/settings/mail").await;
    assert_eq!(initial.status, StatusCode::OK);
    assert_eq!(initial.body["source"], "config");
    assert!(initial.body.get("smtp").is_none());
    assert_eq!(initial.body["can_store_secrets"], true);

    let saved = app
      .put(
        &bob,
        "/admin/settings/mail",
        smtp_request(
          "localhost",
          2525,
          json!({"action": "set", "value": "smtp-secret-pw"}),
        ),
      )
      .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert_eq!(saved.body["source"], "database");
    assert_eq!(saved.body["smtp"]["has_password"], true);
    assert!(!saved.body.to_string().contains("smtp-secret-pw"));
    let row: (String, Vec<u8>) = teamcraft_db::testing::sqlx::query_as(
      "SELECT value::text, sealed_secret FROM server_settings WHERE name = 'mail.smtp'",
    )
    .fetch_one(app.sql_pool())
    .await
    .unwrap();
    assert!(!row.0.contains("smtp-secret-pw"));
    assert!(!row.1.windows(14).any(|window| window == b"smtp-secret-pw"));

    // Keeping the password keeps the sealed bytes.
    let kept = app
      .put(
        &bob,
        "/admin/settings/mail",
        smtp_request("localhost", 2526, json!({"action": "keep"})),
      )
      .await;
    assert_eq!(kept.body["smtp"]["has_password"], true);
    assert_eq!(kept.body["smtp"]["port"], 2526);
    let cleared = app
      .put(
        &bob,
        "/admin/settings/mail",
        smtp_request("localhost", 2526, json!({"action": "clear"})),
      )
      .await;
    assert_eq!(cleared.body["smtp"]["has_password"], false);

    let audit = app.get(&bob, "/admin/audit?action=mail_settings_updated").await;
    assert_eq!(audit.body["entries"].as_array().unwrap().len(), 3);
    assert!(!audit.body.to_string().contains("smtp-secret-pw"));

    let reset = app.delete(&bob, "/admin/settings/mail").await;
    assert_eq!(reset.body["source"], "config");
    assert!(!app.state.mailer.is_set(), "no mail server in the configuration");
    app.stop().await;
  }

  #[tokio::test]
  async fn unsafe_or_broken_settings_are_refused() {
    let app = app!(TestApp::start());
    let bob = admin();
    let plain_remote = app
      .put(
        &bob,
        "/admin/settings/mail",
        smtp_request("smtp.example.com", 25, json!({"action": "keep"})),
      )
      .await;
    assert_eq!(plain_remote.status, StatusCode::BAD_REQUEST);
    let mut injected = smtp_request("localhost", 25, json!({"action": "keep"}));
    injected["from_address"] = json!("a@example.com\r\nBcc: b@example.com");
    assert_eq!(
      app.put(&bob, "/admin/settings/mail", injected).await.status,
      StatusCode::BAD_REQUEST
    );
    // Without a mail server there is nothing to test.
    let test = app.post(&bob, "/admin/settings/mail/test", json!({})).await;
    assert_eq!(test.status, StatusCode::SERVICE_UNAVAILABLE);
    app.stop().await;
  }

  #[tokio::test]
  async fn passwords_need_the_settings_key() {
    let mut app = app!(TestApp::start());
    app.state.maybe_settings_key = None;
    app.reconfigure(|_| {});
    let bob = admin();
    let refused = app
      .put(
        &bob,
        "/admin/settings/mail",
        smtp_request("localhost", 25, json!({"action": "set", "value": "pw"})),
      )
      .await;
    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
    let without = app
      .put(
        &bob,
        "/admin/settings/mail",
        smtp_request("localhost", 25, json!({"action": "clear"})),
      )
      .await;
    assert_eq!(without.status, StatusCode::OK);
    assert_eq!(without.body["can_store_secrets"], false);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_failing_test_mail_is_reported_with_the_error() {
    let app = app!(TestApp::start());
    let bob = admin();
    // Nothing listens on port 1.
    app
      .put(
        &bob,
        "/admin/settings/mail",
        smtp_request("127.0.0.1", 1, json!({"action": "clear"})),
      )
      .await;
    let test = app
      .post(
        &bob,
        "/admin/settings/mail/test",
        json!({"to_address": "carol@example.com"}),
      )
      .await;
    assert_eq!(test.status, StatusCode::OK);
    assert_eq!(test.body["is_ok"], false);
    assert!(test.body["error"].as_str().is_some());
    let status = app.get(&bob, "/admin/settings/mail").await.body["status"].clone();
    assert!(
      status.get("last_failure_at").is_some() && status.get("last_error").is_some(),
      "{status}"
    );
    assert!(status.get("last_success_at").is_none());
    let bad_address = app
      .post(&bob, "/admin/settings/mail/test", json!({"to_address": "nobody"}))
      .await;
    assert_eq!(bad_address.status, StatusCode::BAD_REQUEST);
    assert!(
      audit_actions(&app, &bob)
        .await
        .contains(&"mail_test_sent".to_owned())
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_test_mail_reaches_the_dev_mail_catcher() {
    if !is_reachable(&format!("127.0.0.1:{MAILPIT_PORT}")).await {
      eprintln!("skipping: no mail catcher on port {MAILPIT_PORT}");
      return;
    }
    let app = app!(TestApp::start());
    let bob = admin();
    app
      .put(
        &bob,
        "/admin/settings/mail",
        smtp_request("127.0.0.1", MAILPIT_PORT, json!({"action": "clear"})),
      )
      .await;
    let test = app.post(&bob, "/admin/settings/mail/test", json!({})).await;
    assert_eq!(test.body["is_ok"], true, "{}", test.body);
    let status = app.get(&bob, "/admin/settings/mail").await.body["status"].clone();
    assert!(status.get("last_success_at").is_some(), "{status}");
    app.stop().await;
  }
}

mod calendar_settings {
  use super::*;

  async fn with_meetings() -> Option<TestApp> {
    TestApp::start_with_meetings(
      &meetings_config(),
      Arc::new(FakeCalendar::default()),
      Arc::new(FakeMailer::default()),
    )
    .await
  }

  fn caldav_request(url: &str, password: Value) -> Value {
    json!({
      "calendar_url": url,
      "username": "teamcraft",
      "password": password,
      "free_busy_url_template": "http://127.0.0.1:5232/dav/calendars/{email}/personal/",
    })
  }

  #[tokio::test]
  async fn without_meetings_there_is_no_calendar() {
    let app = app!(TestApp::start());
    let bob = admin();
    let got = app.get(&bob, "/admin/settings/calendar").await;
    assert_eq!(got.body["is_available"], false);
    let put = app
      .put(
        &bob,
        "/admin/settings/calendar",
        caldav_request("http://127.0.0.1:1/dav/", json!({"action": "set", "value": "pw"})),
      )
      .await;
    assert_eq!(put.status, StatusCode::SERVICE_UNAVAILABLE);
    app.stop().await;
  }

  #[tokio::test]
  async fn the_calendar_is_replaced_tested_and_reset() {
    let app = app!(with_meetings());
    let bob = admin();
    let initial = app.get(&bob, "/admin/settings/calendar").await;
    assert_eq!(initial.body["is_available"], true);
    assert_eq!(initial.body["source"], "config");
    // The fake calendar of the test server answers the test.
    let fake = app.post(&bob, "/admin/settings/calendar/test", json!({})).await;
    assert_eq!(fake.body["is_ok"], true);

    for (url, password) in [
      (
        "http://cal.example.com/dav/",
        json!({"action": "set", "value": "pw"}),
      ),
      ("http://127.0.0.1:1/dav/", json!({"action": "clear"})),
      ("http://127.0.0.1:1/dav/", json!({"action": "keep"})),
    ] {
      let refused = app
        .put(&bob, "/admin/settings/calendar", caldav_request(url, password))
        .await;
      assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{url}");
    }
    let saved = app
      .put(
        &bob,
        "/admin/settings/calendar",
        caldav_request(
          "http://127.0.0.1:1/dav/",
          json!({"action": "set", "value": "cal-secret-pw"}),
        ),
      )
      .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert_eq!(saved.body["source"], "database");
    assert_eq!(saved.body["caldav"]["has_password"], true);
    assert!(!saved.body.to_string().contains("cal-secret-pw"));
    let failing = app.post(&bob, "/admin/settings/calendar/test", json!({})).await;
    assert_eq!(failing.body["is_ok"], false);
    assert!(failing.body["error"].as_str().is_some());

    let reset = app.delete(&bob, "/admin/settings/calendar").await;
    assert_eq!(reset.body["source"], "config");
    let actions = audit_actions(&app, &bob).await;
    for action in [
      "calendar_settings_updated",
      "calendar_test",
      "calendar_settings_reset",
    ] {
      assert!(
        actions.iter().any(|a| a == action),
        "{action} missing in {actions:?}"
      );
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn the_dev_calendar_server_passes_the_test() {
    // CalCraft with the account of `dev/init-calcraft.sh`.
    let probe = reqwest::Client::new()
      .get(CALCRAFT_URL)
      .basic_auth("teamcraft", Some("teamcraft-dev-only"))
      .send()
      .await;
    if !probe.is_ok_and(|response| response.status().is_success()) {
      eprintln!("skipping: no CalCraft account at {CALCRAFT_URL}");
      return;
    }
    let app = app!(with_meetings());
    let bob = admin();
    app
      .put(
        &bob,
        "/admin/settings/calendar",
        caldav_request(
          CALCRAFT_URL,
          json!({"action": "set", "value": "teamcraft-dev-only"}),
        ),
      )
      .await;
    let test = app.post(&bob, "/admin/settings/calendar/test", json!({})).await;
    assert_eq!(test.body["is_ok"], true, "{}", test.body);
    app.stop().await;
  }
}

mod instance_settings {
  use super::*;

  #[tokio::test]
  async fn a_group_is_stored_in_force_and_reset_to_the_configuration() {
    let app = app!(TestApp::start());
    let bob = admin();
    let initial = app.get(&bob, "/admin/settings").await;
    assert_eq!(initial.status, StatusCode::OK);
    assert_eq!(initial.body["integration"]["default_retention_days"], 30);
    assert!(
      initial.body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .all(|state| state["source"] == "config")
    );

    let saved = app
      .put(
        &bob,
        "/admin/settings",
        json!({"section": "integration", "values": {"default_retention_days": 7}}),
      )
      .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert_eq!(saved.body["integration"]["default_retention_days"], 7);
    let integration = saved.body["sources"]
      .as_array()
      .unwrap()
      .iter()
      .find(|state| state["key"] == "integration")
      .unwrap();
    assert_eq!(integration["source"], "database");
    assert!(integration.get("updated_at").is_some());
    assert_eq!(app.state.settings.get().integration.default_retention_days, 7);
    // The other groups keep the configuration's values.
    assert_eq!(
      app.state.settings.get().notifications.urgent_max_pushes,
      app.state.settings.base().notifications.urgent_max_pushes
    );

    let reset = app.delete(&bob, "/admin/settings/integration").await;
    assert_eq!(reset.body["integration"]["default_retention_days"], 30);
    assert_eq!(app.state.settings.get().integration.default_retention_days, 30);
    let actions = audit_actions(&app, &bob).await;
    assert!(
      actions.contains(&"settings_updated".to_owned()) && actions.contains(&"settings_reset".to_owned())
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn invalid_values_and_unknown_groups_are_refused() {
    let app = app!(TestApp::start());
    let bob = admin();
    let current = app.get(&bob, "/admin/settings").await.body;
    let mut shares = current["shares"].clone();
    shares["default_expiry_days"] = json!(shares["max_expiry_days"].as_u64().unwrap() + 1);
    let refused = app
      .put(
        &bob,
        "/admin/settings",
        json!({"section": "shares", "values": shares}),
      )
      .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    let mut texts = current["mail_texts"].clone();
    texts["invite_subject"] = json!("Hello\r\nBcc: x@example.com");
    let injected = app
      .put(
        &bob,
        "/admin/settings",
        json!({"section": "mail_texts", "values": texts}),
      )
      .await;
    assert_eq!(injected.status, StatusCode::BAD_REQUEST);
    let unknown = app
      .put(
        &bob,
        "/admin/settings",
        json!({"section": "telephony", "values": {}}),
      )
      .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    assert_eq!(
      app.delete(&bob, "/admin/settings/telephony").await.status,
      StatusCode::BAD_REQUEST
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn changed_texts_reach_the_invitation_mail() {
    let mailer = Arc::new(FakeMailer::default());
    let app = app!(TestApp::start_with_mailer(|_| {}, mailer.clone()));
    let bob = admin();
    let mut texts = app.get(&bob, "/admin/settings").await.body["mail_texts"].clone();
    texts["invite_subject"] = json!("Come and join {space}");
    let saved = app
      .put(
        &bob,
        "/admin/settings",
        json!({"section": "mail_texts", "values": texts}),
      )
      .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    let space = create_space(&app, &bob, "Project A", "invite_only").await;
    let invited = app
      .post(
        &bob,
        &format!("/spaces/{space}/invites"),
        json!({"email": "dana@example.org", "role": "member"}),
      )
      .await;
    assert!(invited.status.is_success(), "{}", invited.body);
    let sent = mailer.sent();
    assert!(
      sent.iter().any(|mail| mail.subject == "Come and join Project A"),
      "{:?}",
      sent.iter().map(|mail| &mail.subject).collect::<Vec<_>>()
    );
    app.stop().await;
  }
}
