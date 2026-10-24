//! The directory tells who hasn't set up the app yet (`has_devices`), for colleagues the caller
//! shares nothing with too.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use serde_json::{Value, json};
use teamcraft_server::testing::TestApp;

#[tokio::test]
async fn the_directory_says_who_has_a_device() {
  let Some(app) = TestApp::start().await else { return };
  let (carol, dave, erin) = (member("carol"), member("dave"), member("erin"));
  // Carol shares no space or conversation with either of them.
  user_id(&app, &carol).await;
  let dave_id = user_id(&app, &dave).await;
  let erin_id = user_id(&app, &erin).await;
  register_device(&app, &erin).await;

  let found = app.get(&carol, "/users?query=").await;
  let has_devices = |name: &str| {
    found.body["users"]
      .as_array()
      .unwrap()
      .iter()
      .find(|user| user["username"] == name)
      .map(|user| user["has_devices"].clone())
  };
  assert_eq!(has_devices("dave"), Some(json!(false)));
  assert_eq!(has_devices("erin"), Some(json!(true)));

  assert_eq!(
    app.get(&carol, &format!("/users/{dave_id}")).await.body["has_devices"],
    json!(false)
  );
  assert_eq!(
    app.get(&carol, &format!("/users/{erin_id}")).await.body["has_devices"],
    Value::Bool(true)
  );
  app.stop().await;
}

#[tokio::test]
async fn the_query_narrows_the_directory_and_special_characters_match_literally() {
  let Some(app) = TestApp::start().await else { return };
  let (carol, dave) = (member("carol"), member("dave"));
  user_id(&app, &carol).await;
  user_id(&app, &dave).await;

  let found = app.get(&carol, "/users?query=dav").await;
  let names: Vec<&str> = found.body["users"]
    .as_array()
    .unwrap()
    .iter()
    .filter_map(|user| user["username"].as_str())
    .collect();
  assert_eq!(names, vec!["dave"]);
  // `%` is a LIKE wildcard in SQL; here it is only a character nobody's name contains.
  let none = app.get(&carol, "/users?query=%25").await;
  assert_eq!(none.body["users"], json!([]));
  app.stop().await;
}
