//! Organizations: read from a configured token claim, shown on users, filterable in the directory.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_server::testing::{TestApp, TestUser};

const CLAIM: &str = "organization";

/// A request as `user` whose token carries these extra claims.
async fn get_with_claims(app: &TestApp, user: &TestUser, claims: Value, path: &str) -> (StatusCode, Value) {
  let issued_at = i64::try_from(jsonwebtoken::get_current_timestamp()).unwrap();
  let token = app.token_with(user, issued_at, &claims);
  let response = app
    .http
    .get(format!("{}/api/v1{path}", app.base_url))
    .bearer_auth(token)
    .send()
    .await
    .unwrap();
  let status = response.status();
  (status, response.json().await.unwrap_or(Value::Null))
}

async fn app_with_claim() -> Option<TestApp> {
  TestApp::start_with(|settings| settings.maybe_organization_claim = Some(CLAIM.into())).await
}

#[tokio::test]
async fn the_claim_becomes_the_users_organization_and_is_shown_to_others() {
  let Some(app) = app_with_claim().await else { return };
  let (carol, dave, erin) = (member("carol"), member("dave"), member("erin"));
  let (status, _) = get_with_claims(&app, &carol, json!({CLAIM: "Brand A GmbH"}), "/me").await;
  assert_eq!(status, StatusCode::OK);
  // A list claim counts by its first entry; whitespace is trimmed.
  get_with_claims(&app, &dave, json!({CLAIM: ["  Brand B AG ", "Other"]}), "/me").await;
  get_with_claims(&app, &erin, json!({}), "/me").await;

  let found = app.get(&erin, "/users?query=").await;
  let organization_of = |name: &str| {
    found.body["users"]
      .as_array()
      .unwrap()
      .iter()
      .find(|user| user["username"] == name)
      .map(|user| user["organization"].clone())
  };
  assert_eq!(organization_of("carol"), Some(json!("Brand A GmbH")));
  assert_eq!(organization_of("dave"), Some(json!("Brand B AG")));
  assert_eq!(organization_of("erin"), Some(Value::Null));

  let filtered = app.get(&erin, "/users?query=&organization=Brand%20B%20AG").await;
  let names: Vec<&str> = filtered.body["users"]
    .as_array()
    .unwrap()
    .iter()
    .map(|user| user["username"].as_str().unwrap())
    .collect();
  assert_eq!(names, ["dave"]);

  let organizations = app.get(&erin, "/organizations").await;
  assert_eq!(
    organizations.body["organizations"],
    json!(["Brand A GmbH", "Brand B AG"])
  );
  app.stop().await;
}

#[tokio::test]
async fn a_changed_or_missing_claim_updates_the_stored_organization() {
  let Some(app) = app_with_claim().await else { return };
  let (carol, dave) = (member("carol"), member("dave"));
  get_with_claims(&app, &carol, json!({CLAIM: "Brand A GmbH"}), "/me").await;
  get_with_claims(&app, &carol, json!({CLAIM: "Brand B AG"}), "/me").await;
  let found = app.get(&dave, "/users?query=carol").await;
  assert_eq!(found.body["users"][0]["organization"], "Brand B AG");
  get_with_claims(&app, &carol, json!({}), "/me").await;
  let found = app.get(&dave, "/users?query=carol").await;
  assert!(found.body["users"][0].get("organization").is_none());
  app.stop().await;
}

#[tokio::test]
async fn unusable_claim_values_are_ignored() {
  let Some(app) = app_with_claim().await else { return };
  let (carol, dave, viewer) = (member("carol"), member("dave"), member("viewer"));
  let too_long = "x".repeat(101);
  for (user, value) in [(&carol, json!(too_long)), (&dave, json!({"nested": "no"}))] {
    get_with_claims(&app, user, json!({CLAIM: value}), "/me").await;
  }
  for name in ["carol", "dave"] {
    let found = app.get(&viewer, &format!("/users?query={name}")).await;
    assert!(found.body["users"][0].get("organization").is_none(), "{name}");
  }
  app.stop().await;
}

#[tokio::test]
async fn without_a_configured_claim_nobody_has_an_organization() {
  let Some(app) = TestApp::start().await else { return };
  let carol = member("carol");
  get_with_claims(&app, &carol, json!({CLAIM: "Brand A GmbH"}), "/me").await;
  let found = app.get(&carol, "/users?query=carol").await;
  assert!(found.body["users"][0].get("organization").is_none());
  assert_eq!(
    app.get(&carol, "/organizations").await.body["organizations"],
    json!([])
  );
  app.stop().await;
}

#[tokio::test]
async fn guests_get_no_organization_list() {
  let Some(app) = app_with_claim().await else { return };
  let (carol, visitor) = (member("carol"), guest("visitor"));
  get_with_claims(&app, &carol, json!({CLAIM: "Brand A GmbH"}), "/me").await;
  let reply = app.get(&visitor, "/organizations").await;
  assert_eq!(reply.status, StatusCode::OK);
  assert_eq!(reply.body["organizations"], json!([]));
  app.stop().await;
}
