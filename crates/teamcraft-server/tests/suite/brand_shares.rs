//! Share links belong to the brand that made them (docs/branding.md §8). Needs the dev Garage for
//! the files, like the share tests.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_server::config::BrandConfig;
use teamcraft_server::testing::TestApp;

const HOST_A: &str = "chat.a.example.com";
const HOST_B: &str = "chat.b.example.com";

fn brand(id: &str, host: &str, is_default: bool) -> BrandConfig {
  BrandConfig {
    id: id.into(),
    is_default,
    hostnames: vec![host.into()],
    display_name: format!("Brand {id}"),
    ..BrandConfig::default()
  }
}

async fn public(app: &TestApp, host: &str, method: reqwest::Method, path: &str) -> (StatusCode, Value) {
  let response = app
    .http
    .request(method, format!("{}/api/v1{path}", app.base_url))
    .header("Host", host)
    .send()
    .await
    .unwrap();
  let status = response.status();
  (status, response.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn a_link_works_only_on_the_hosts_of_the_brand_that_made_it() {
  let Some(app) = TestApp::start_with_storage(|settings| {
    settings.brands = vec![brand("a", HOST_A, true), brand("b", HOST_B, false)];
    settings.shares.public_base_url = "https://configured.example.com".into();
  })
  .await
  else {
    return;
  };
  let (carol, dave) = (member("carol"), member("dave"));
  let channel = channel_with(&app, &[&carol, &dave]).await;
  let created = app
    .post(
      &carol,
      "/attachments",
      json!({"conversation_id": channel, "size_bytes": 4}),
    )
    .await;
  let attachment = created.id();
  let put = app
    .http
    .put(created.body["upload_url"].as_str().unwrap())
    .body(b"data".to_vec())
    .send()
    .await
    .unwrap();
  assert_eq!(put.status(), StatusCode::OK);
  let done = app
    .post(&carol, &format!("/attachments/{attachment}/complete"), json!({}))
    .await;
  assert_eq!(done.status, StatusCode::OK, "{}", done.body);

  // Made through brand B's host name.
  let token = app.token(&carol);
  let response = app
    .http
    .post(format!("{}/api/v1/shares", app.base_url))
    .header("Host", HOST_B)
    .bearer_auth(token)
    .json(&json!({"attachment_id": attachment}))
    .send()
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::CREATED);
  let link: Value = response.json().await.unwrap();
  let id = link["id"].as_str().unwrap();
  // The link names a host of its brand, not the received host and not the configured base.
  assert_eq!(link["url"], format!("https://{HOST_B}/f/{id}"));

  let info_path = format!("/shares/{id}");
  let (status, body) = public(&app, HOST_B, reqwest::Method::GET, &info_path).await;
  assert_eq!(status, StatusCode::OK, "{body}");
  for host in [HOST_A, "evil.example", "chat.b.example.com.evil.example"] {
    let (status, body) = public(&app, host, reqwest::Method::GET, &info_path).await;
    assert_eq!(
      (status, &body["error_code"]),
      (StatusCode::GONE, &json!("share_unavailable")),
      "{host}"
    );
    let (status, _) = public(
      &app,
      host,
      reqwest::Method::POST,
      &format!("/shares/{id}/download"),
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{host}");
  }
  let (status, _) = public(
    &app,
    HOST_B,
    reqwest::Method::POST,
    &format!("/shares/{id}/download"),
  )
  .await;
  assert_eq!(status, StatusCode::OK);
  app.stop().await;
}
