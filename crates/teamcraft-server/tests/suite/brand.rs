//! Brands: the host name selects the brand, unknown hosts, assets, theme and per-brand policy.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value;
use teamcraft_server::config::{BrandColorsConfig, BrandConfig, UnknownHostPolicy};
use teamcraft_server::testing::TestApp;

const HOST_A: &str = "chat.a.example.com";
const HOST_B: &str = "chat.b.example.com";
const LOGO_B: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>";

struct Files {
  dir: PathBuf,
}

impl Files {
  fn create() -> Self {
    let dir = std::env::temp_dir().join(format!("teamcraft-brand-it-{}-{}", std::process::id(), nanos()));
    std::fs::create_dir_all(dir.join("site")).unwrap();
    std::fs::create_dir_all(dir.join("brands/b")).unwrap();
    std::fs::write(dir.join("site/index.html"), "<!doctype html><title>x</title>").unwrap();
    std::fs::write(dir.join("brands/b/logo.svg"), LOGO_B).unwrap();
    Self { dir }
  }
}

impl Drop for Files {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.dir);
  }
}

fn nanos() -> u128 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_nanos()
}

fn brand(id: &str, host: &str, is_default: bool) -> BrandConfig {
  BrandConfig {
    id: id.into(),
    is_default,
    hostnames: vec![host.into()],
    display_name: format!("Brand {}", id.to_uppercase()),
    ..BrandConfig::default()
  }
}

async fn start(files: &Files, unknown: UnknownHostPolicy) -> Option<TestApp> {
  let dir = files.dir.clone();
  TestApp::start_with(move |settings| {
    settings.web.maybe_static_dir = Some(dir.join("site"));
    settings.branding.maybe_assets_dir = Some(dir.join("brands"));
    settings.branding.unknown_host = unknown;
    let a = brand("a", HOST_A, true);
    let mut b = brand("b", HOST_B, false);
    b.maybe_logo = Some("b/logo.svg".into());
    b.colors = BrandColorsConfig {
      light: [("accent".to_owned(), "#112233".to_owned())].into(),
      dark: [("accent".to_owned(), "#445566".to_owned())].into(),
    };
    b.connect_origins = vec!["https://api.b.example.com".into()];
    b.login_title = [("en".to_owned(), "Welcome to B".to_owned())].into();
    settings.brands = vec![a, b];
  })
  .await
}

async fn get_as(app: &TestApp, host: Option<&str>, path: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
  let mut request = app.http.get(format!("{}{path}", app.base_url));
  if let Some(host) = host {
    request = request.header("Host", host);
  }
  let response = request.send().await.unwrap();
  let (status, headers) = (response.status(), response.headers().clone());
  (status, headers, response.bytes().await.unwrap().to_vec())
}

fn json(body: &[u8]) -> Value {
  serde_json::from_slice(body).unwrap()
}

#[tokio::test]
async fn without_brands_the_builtin_default_answers_on_any_host() {
  let Some(app) = TestApp::start().await else { return };
  let (status, _, body) = get_as(&app, Some("anything.example.com"), "/api/v1/brand").await;
  assert_eq!(status, StatusCode::OK);
  let brand = json(&body);
  assert_eq!(brand["id"], "teamcraft");
  assert_eq!(brand["display_name"], "TeamCraft");
  app.stop().await;
}

#[tokio::test]
async fn the_host_selects_the_brand() {
  let files = Files::create();
  let Some(app) = start(&files, UnknownHostPolicy::Default).await else {
    return;
  };

  let (status, headers, body) = get_as(&app, Some(HOST_B), "/api/v1/brand").await;
  assert_eq!(status, StatusCode::OK);
  let brand = json(&body);
  assert_eq!(brand["id"], "b");
  assert_eq!(brand["origin"], "https://chat.b.example.com");
  assert_eq!(brand["login_title"]["en"], "Welcome to B");
  assert_eq!(brand["assets"]["logo"], "/api/v1/brand/assets/logo");
  assert_eq!(headers["vary"], "Host, X-Forwarded-Host");
  assert!(headers["cache-control"].to_str().unwrap().starts_with("public"));

  let (_, _, body) = get_as(&app, Some(HOST_A), "/api/v1/brand").await;
  assert_eq!(json(&body)["id"], "a");
  // Port and case do not matter.
  let (_, _, body) = get_as(&app, Some("CHAT.B.example.com:8443"), "/api/v1/brand").await;
  assert_eq!(json(&body)["id"], "b");
  app.stop().await;
}

#[tokio::test]
async fn an_unknown_host_gets_the_default_brand_and_is_never_reflected() {
  let files = Files::create();
  let Some(app) = start(&files, UnknownHostPolicy::Default).await else {
    return;
  };
  for host in [
    "evil.example",
    "chat.b.example.com.evil.example",
    "evil-chat.b.example.com",
  ] {
    for path in ["/api/v1/brand", "/api/v1/brand/theme.css", "/"] {
      let (status, headers, body) = get_as(&app, Some(host), path).await;
      assert_eq!(status, StatusCode::OK, "{host} {path}");
      let everything = format!("{headers:?}{}", String::from_utf8_lossy(&body));
      assert!(!everything.contains("evil"), "{host} {path}: {everything}");
    }
    let (_, _, body) = get_as(&app, Some(host), "/api/v1/brand").await;
    let brand = json(&body);
    assert_eq!(brand["id"], "a", "{host}");
    assert_eq!(brand["origin"], "https://chat.a.example.com");
  }
  app.stop().await;
}

#[tokio::test]
async fn unknown_hosts_can_be_refused_except_for_probes() {
  let files = Files::create();
  let Some(app) = start(&files, UnknownHostPolicy::Reject).await else {
    return;
  };

  for path in ["/api/v1/brand", "/", "/api/v1/me", "/api/v1/brand/theme.css"] {
    let (status, _, body) = get_as(&app, Some("evil.example"), path).await;
    assert_eq!(status, StatusCode::MISDIRECTED_REQUEST, "{path}");
    assert_eq!(json(&body)["error_code"], "unknown_host", "{path}");
  }
  for path in ["/healthz", "/readyz"] {
    let (status, _, _) = get_as(&app, Some("10.0.0.5:8080"), path).await;
    assert_eq!(status, StatusCode::OK, "{path}");
  }
  let (status, _, _) = get_as(&app, Some(HOST_A), "/api/v1/brand").await;
  assert_eq!(status, StatusCode::OK);
  app.stop().await;
}

#[tokio::test]
async fn assets_come_from_the_request_brand_with_validators() {
  let files = Files::create();
  let Some(app) = start(&files, UnknownHostPolicy::Default).await else {
    return;
  };

  let (status, headers, body) = get_as(&app, Some(HOST_B), "/api/v1/brand/assets/logo").await;
  assert_eq!((status, body.as_slice()), (StatusCode::OK, LOGO_B));
  assert_eq!(headers["content-type"], "image/svg+xml");
  assert!(
    headers["content-security-policy"]
      .to_str()
      .unwrap()
      .contains("default-src 'none'")
  );
  let etag = headers["etag"].to_str().unwrap().to_owned();

  let response = app
    .http
    .get(format!("{}/api/v1/brand/assets/logo", app.base_url))
    .header("Host", HOST_B)
    .header("If-None-Match", &etag)
    .send()
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

  // Brand A has no logo; unknown slots and path tricks never reach the file system.
  for (host, slot) in [
    (HOST_A, "logo"),
    (HOST_B, "icon"),
    (HOST_B, "hero"),
    (HOST_B, "..%2Fsite%2Findex.html"),
    (HOST_B, "logo.svg"),
  ] {
    let (status, _, body) = get_as(&app, Some(host), &format!("/api/v1/brand/assets/{slot}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{host} {slot}");
    assert_eq!(json(&body)["error_code"], "brand_asset_not_found", "{slot}");
  }
  app.stop().await;
}

#[tokio::test]
async fn the_theme_is_a_stylesheet_of_the_brands_colors() {
  let files = Files::create();
  let Some(app) = start(&files, UnknownHostPolicy::Default).await else {
    return;
  };
  let (status, headers, body) = get_as(&app, Some(HOST_B), "/api/v1/brand/theme.css").await;
  assert_eq!(status, StatusCode::OK);
  assert!(headers["content-type"].to_str().unwrap().starts_with("text/css"));
  let css = String::from_utf8(body).unwrap();
  assert!(
    css.contains("--accent:#112233;") && css.contains("--accent:#445566;"),
    "{css}"
  );
  let (_, _, body) = get_as(&app, Some(HOST_A), "/api/v1/brand/theme.css").await;
  assert!(body.is_empty());
  app.stop().await;
}

#[tokio::test]
async fn the_web_client_gets_the_policy_of_its_brand() {
  let files = Files::create();
  let Some(app) = start(&files, UnknownHostPolicy::Default).await else {
    return;
  };
  let (_, headers_b, _) = get_as(&app, Some(HOST_B), "/").await;
  let (_, headers_a, _) = get_as(&app, Some(HOST_A), "/").await;
  let policy = |headers: &HeaderMap| headers["content-security-policy"].to_str().unwrap().to_owned();
  assert!(
    policy(&headers_b).contains("https://api.b.example.com"),
    "{}",
    policy(&headers_b)
  );
  assert!(
    !policy(&headers_a).contains("api.b.example.com"),
    "{}",
    policy(&headers_a)
  );
  app.stop().await;
}

mod mail {
  use std::sync::Arc;

  use serde_json::json;
  use teamcraft_server::testing::FakeMailer;

  use super::*;

  async fn invite_as(app: &TestApp, host: &str, email: &str) {
    // The inviter's address belongs to brand A: that is their home brand.
    let alice = TestUser::new("alice", &["owner"]).with_email("alice@a-company.example.com", true);
    let space = app
      .post(
        &alice,
        "/spaces",
        json!({"name": "Client", "join_policy": "invite_only"}),
      )
      .await
      .id();
    let response = app
      .http
      .post(format!("{}/api/v1/spaces/{space}/invites", app.base_url))
      .header("Host", host)
      .bearer_auth(app.token(&alice))
      .json(&json!({"email": email, "role": "guest"}))
      .send()
      .await
      .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
  }

  use teamcraft_server::testing::TestUser;

  async fn start(mailer: Arc<FakeMailer>) -> Option<TestApp> {
    TestApp::start_with_mailer(
      |settings| {
        settings.mail.public_base_url = "https://global.example.com".into();
        // The test client calls through the loopback address.
        settings.branding.unknown_host = UnknownHostPolicy::Default;
        let mut a = brand("a", HOST_A, true);
        a.email_domains = vec!["a-company.example.com".into()];
        a.mail.maybe_sender_name = Some("Brand A Team".into());
        let mut b = brand("b", HOST_B, false);
        b.email_domains = vec!["b-company.example.com".into()];
        b.mail.maybe_sender_name = Some("Brand B Team".into());
        b.mail.maybe_from_address = Some("chat@b-company.example.com".into());
        b.mail.maybe_invite_subject = Some("[B] {inviter} invited you to {space}".into());
        settings.brands = vec![a, b];
      },
      mailer,
    )
    .await
  }

  #[tokio::test]
  async fn mail_goes_out_as_the_brand_of_the_recipients_domain_else_of_the_inviter() {
    let mailer = Arc::new(FakeMailer::default());
    let Some(app) = start(mailer.clone()).await else {
      return;
    };

    // The inviter works on brand A's host; the invitee's domain belongs to brand B.
    invite_as(&app, HOST_A, "max@b-company.example.com").await;
    // An unlisted domain gets the brand of the request.
    invite_as(&app, HOST_A, "ext@elsewhere.example.com").await;
    invite_as(&app, HOST_B, "ext2@elsewhere.example.com").await;

    let sent = mailer.sent();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0].maybe_from_name.as_deref(), Some("Brand B Team"));
    assert_eq!(
      sent[0].maybe_from_address.as_deref(),
      Some("chat@b-company.example.com")
    );
    assert!(sent[0].subject.starts_with("[B] "), "{}", sent[0].subject);
    assert!(
      sent[0].text.contains("https://chat.b.example.com"),
      "{}",
      sent[0].text
    );

    assert_eq!(sent[1].maybe_from_name.as_deref(), Some("Brand A Team"));
    assert!(sent[1].maybe_from_address.is_none());
    assert!(
      sent[1].text.contains("https://chat.a.example.com"),
      "{}",
      sent[1].text
    );
    assert!(!sent[1].text.contains("global.example.com"));

    // The Host header the request came in on does not choose the identity of unlisted recipients:
    // the same inviter on brand B's host still sends as brand A.
    assert_eq!(sent[2].maybe_from_name.as_deref(), Some("Brand A Team"));
    assert!(
      sent[2].text.contains("https://chat.a.example.com"),
      "{}",
      sent[2].text
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn without_brand_hosts_the_global_base_url_is_used() {
    let mailer = Arc::new(FakeMailer::default());
    let Some(app) = TestApp::start_with_mailer(
      |settings| settings.mail.public_base_url = "https://global.example.com".into(),
      mailer.clone(),
    )
    .await
    else {
      return;
    };
    invite_as(&app, "anything.example.com", "x@example.org").await;
    let sent = mailer.sent();
    assert!(sent[0].text.contains("https://global.example.com"));
    assert!(sent[0].maybe_from_name.is_none());
    app.stop().await;
  }
}

#[tokio::test]
async fn spaces_can_carry_a_brand_tag_that_names_a_configured_brand() {
  use serde_json::json;
  use teamcraft_server::testing::TestUser;

  let files = Files::create();
  let Some(app) = start(&files, UnknownHostPolicy::Default).await else {
    return;
  };
  let alice = TestUser::new("alice", &["owner"]);
  let space = app
    .post(
      &alice,
      "/spaces",
      json!({"name": "Project", "join_policy": "invite_only"}),
    )
    .await
    .id();

  let tagged = app
    .patch(&alice, &format!("/spaces/{space}"), json!({"brand_id": "b"}))
    .await;
  assert_eq!(tagged.status, StatusCode::OK, "{}", tagged.body);
  assert_eq!(tagged.body["brand_id"], "b");
  assert_eq!(
    app.get(&alice, &format!("/spaces/{space}")).await.body["brand_id"],
    "b"
  );

  let unknown = app
    .patch(&alice, &format!("/spaces/{space}"), json!({"brand_id": "nope"}))
    .await;
  assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
  // Other changes keep the tag; an empty string clears it.
  let renamed = app
    .patch(&alice, &format!("/spaces/{space}"), json!({"name": "Renamed"}))
    .await;
  assert_eq!(renamed.body["brand_id"], "b");
  let cleared = app
    .patch(&alice, &format!("/spaces/{space}"), json!({"brand_id": ""}))
    .await;
  assert!(cleared.body.get("brand_id").is_none(), "{}", cleared.body);

  let brands = app.get(&alice, "/brands").await;
  let ids: Vec<&str> = brands.body["brands"]
    .as_array()
    .unwrap()
    .iter()
    .map(|b| b["id"].as_str().unwrap())
    .collect();
  assert_eq!(ids, ["a", "b"]);
  assert!(brands.body["brands"][1]["colors"]["light"]["accent"].is_string());
  assert!(
    !brands.body.to_string().contains("example.com"),
    "no host names in the list"
  );
  app.stop().await;
}

#[tokio::test]
async fn guests_get_no_brand_list() {
  let files = Files::create();
  let Some(app) = start(&files, UnknownHostPolicy::Default).await else {
    return;
  };
  let visitor = teamcraft_server::testing::TestUser::new("visitor", &["guest"]);
  let reply = app.get(&visitor, "/brands").await;
  assert_eq!(reply.status, StatusCode::OK);
  assert_eq!(reply.body["brands"], serde_json::json!([]));
  app.stop().await;
}

#[tokio::test]
async fn unknown_hosts_are_refused_unless_configured_otherwise() {
  let files = Files::create();
  let dir = files.dir.clone();
  let Some(app) = TestApp::start_with(move |settings| {
    settings.branding.maybe_assets_dir = Some(dir.join("brands"));
    settings.brands = vec![brand("a", HOST_A, true)];
  })
  .await
  else {
    return;
  };
  let (status, _, _) = get_as(&app, Some("evil.example"), "/api/v1/brand").await;
  assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
  let (status, _, _) = get_as(&app, Some(HOST_A), "/api/v1/brand").await;
  assert_eq!(status, StatusCode::OK);
  app.stop().await;
}
