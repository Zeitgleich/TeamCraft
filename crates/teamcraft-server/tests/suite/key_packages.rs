//! MLS key packages against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::collections::HashSet;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_server::testing::{TestApp, TestUser};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

/// Distinct real key packages (base64).
fn packages(count: usize) -> Vec<String> {
  (0..count).map(|_| key_package()).collect()
}

async fn upload(
  app: &TestApp,
  user: &TestUser,
  device: &str,
  regular: &[String],
  last_resort: Option<&str>,
) -> Value {
  let mut body = json!({"key_packages": regular});
  if let Some(last_resort) = last_resort {
    body["last_resort"] = json!(last_resort);
  }
  let reply = app
    .post(user, &format!("/devices/{device}/key-packages"), body)
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body
}

async fn claim(app: &TestApp, by: &TestUser, user: &str, channel: &str) -> teamcraft_server::testing::Reply {
  app
    .post(
      by,
      &format!("/users/{user}/key-packages/claim"),
      json!({"conversation_id": channel}),
    )
    .await
}

#[tokio::test]
async fn uploads_are_counted_and_the_last_resort_is_replaced() {
  let app = app!();
  let carol = member("carol");
  let device = register_device(&app, &carol).await;

  let status = upload(
    &app,
    &carol,
    &device,
    &packages(3),
    Some(&last_resort_key_package()),
  )
  .await;
  assert_eq!(
    status,
    json!({"available": 3, "expiring_soon": 0, "has_last_resort": true})
  );
  let status = upload(
    &app,
    &carol,
    &device,
    &packages(2),
    Some(&last_resort_key_package()),
  )
  .await;
  assert_eq!(status["available"], 5);
  let status = app.get(&carol, &format!("/devices/{device}/key-packages")).await;
  assert_eq!(status.body["has_last_resort"], true);
  app.stop().await;
}

#[tokio::test]
async fn uploads_are_validated_and_limited() {
  let Some(app) = TestApp::start_with(|s| {
    s.limits.max_key_packages_per_device = 3;
    s.limits.max_key_packages_per_upload = 2;
  })
  .await
  else {
    return;
  };
  let (carol, dave) = (member("carol"), member("dave"));
  let device = register_device(&app, &carol).await;
  let path = format!("/devices/{device}/key-packages");

  assert_eq!(
    app
      .post(&carol, &path, json!({"key_packages": packages(3)}))
      .await
      .status,
    StatusCode::BAD_REQUEST
  );
  assert_eq!(
    app
      .post(&carol, &path, json!({"key_packages": [""]}))
      .await
      .status,
    StatusCode::BAD_REQUEST
  );
  upload(&app, &carol, &device, &packages(2), None).await;
  upload(&app, &carol, &device, &packages(1), None).await;
  let over = app
    .post(&carol, &path, json!({"key_packages": packages(1)}))
    .await;
  assert_eq!(over.status, StatusCode::CONFLICT);

  // Another user's device is not yours.
  assert_eq!(
    app
      .post(&dave, &path, json!({"key_packages": packages(1)}))
      .await
      .error_code(),
    "device_not_found"
  );
  assert_eq!(app.get(&dave, &path).await.status, StatusCode::NOT_FOUND);
  app.stop().await;
}

#[tokio::test]
async fn claiming_takes_one_package_per_device_then_falls_back_to_the_last_resort() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let channel = channel_with(&app, &[&carol, &dave]).await;
  let dave_id = user_id(&app, &dave).await;
  let (phone, laptop) = (
    register_device(&app, &dave).await,
    register_device(&app, &dave).await,
  );
  let phone_last_resort = last_resort_key_package();
  upload(&app, &dave, &phone, &packages(1), Some(&phone_last_resort)).await;
  upload(&app, &dave, &laptop, &packages(2), None).await;

  let first = claim(&app, &carol, &dave_id, &channel).await;
  assert_eq!(first.status, StatusCode::OK, "{}", first.body);
  assert_eq!(first.body["packages"].as_array().unwrap().len(), 2);
  assert!(
    first.body["packages"]
      .as_array()
      .unwrap()
      .iter()
      .all(|p| p["is_last_resort"] == false)
  );

  // The phone is out of regular packages: its last-resort package is handed out (and stays).
  let second = claim(&app, &carol, &dave_id, &channel).await;
  let by_device = |reply: &teamcraft_server::testing::Reply, device: &str| {
    reply.body["packages"]
      .as_array()
      .unwrap()
      .iter()
      .find(|p| p["device_id"] == device)
      .cloned()
  };
  assert_eq!(by_device(&second, &phone).unwrap()["is_last_resort"], true);
  assert_eq!(
    by_device(&second, &phone).unwrap()["key_package"],
    phone_last_resort.as_str()
  );
  assert!(by_device(&second, &laptop).is_some());
  let third = claim(&app, &carol, &dave_id, &channel).await;
  assert_eq!(by_device(&third, &phone).unwrap()["is_last_resort"], true);
  assert!(
    by_device(&third, &laptop).is_none(),
    "the laptop has nothing left"
  );

  // Nothing at all left for a user without any package.
  let carol_id = user_id(&app, &carol).await;
  register_device(&app, &carol).await;
  let exhausted = app
    .post(
      &dave,
      &format!("/users/{carol_id}/key-packages/claim"),
      json!({"conversation_id": channel}),
    )
    .await;
  assert_eq!(exhausted.status, StatusCode::CONFLICT);
  assert_eq!(exhausted.error_code(), "key_package_exhausted");
  app.stop().await;
}

#[tokio::test]
async fn concurrent_claims_never_hand_out_the_same_package() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let channel = channel_with(&app, &[&carol, &dave]).await;
  let dave_id = user_id(&app, &dave).await;
  let device = register_device(&app, &dave).await;
  upload(&app, &dave, &device, &packages(20), None).await;

  let claims = (0..20).map(|_| {
    let (app, carol, channel, dave_id) = (&app, &carol, channel.clone(), dave_id.clone());
    async move {
      let reply = app
        .post(
          carol,
          &format!("/users/{dave_id}/key-packages/claim"),
          json!({"conversation_id": channel}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
      reply.body["packages"][0]["key_package"]
        .as_str()
        .unwrap()
        .to_owned()
    }
  });
  let claimed: Vec<String> = futures_util::future::join_all(claims).await;
  assert_eq!(
    claimed.iter().collect::<HashSet<_>>().len(),
    20,
    "a package was handed out twice"
  );
  assert_eq!(
    app
      .get(&dave, &format!("/devices/{device}/key-packages"))
      .await
      .body["available"],
    0
  );
  app.stop().await;
}

#[tokio::test]
async fn only_members_claim_for_members() {
  let app = app!();
  let (carol, dave, erin) = (member("carol"), member("dave"), member("erin"));
  let channel = channel_with(&app, &[&carol, &dave]).await;
  let (dave_id, erin_id) = (user_id(&app, &dave).await, user_id(&app, &erin).await);
  let device = register_device(&app, &dave).await;
  upload(&app, &dave, &device, &packages(2), None).await;
  let body = json!({"conversation_id": channel});

  // erin is no member: she can't even see the channel.
  assert_eq!(
    app
      .post(
        &erin,
        &format!("/users/{dave_id}/key-packages/claim"),
        body.clone()
      )
      .await
      .status,
    StatusCode::NOT_FOUND
  );
  // The target must be a member too.
  assert_eq!(
    app
      .post(&carol, &format!("/users/{erin_id}/key-packages/claim"), body)
      .await
      .status,
    StatusCode::BAD_REQUEST
  );
  assert_eq!(
    app
      .get(&dave, &format!("/devices/{device}/key-packages"))
      .await
      .body["available"],
    2
  );
  app.stop().await;
}

#[tokio::test]
async fn invalid_or_nearly_expired_packages_are_refused() {
  let app = app!();
  let carol = member("carol");
  let device = register_device(&app, &carol).await;
  let path = format!("/devices/{device}/key-packages");
  let day = 24 * 60 * 60;
  let refused = [
    json!({"key_packages": ["bm90IGEga2V5IHBhY2thZ2U="]}),
    // Made 50 days ago: 33 days left, less than the 42 a served package must keep.
    json!({"key_packages": [key_package_at(now_secs() - 50 * day, false)]}),
    json!({"key_packages": [last_resort_key_package()]}),
    json!({"last_resort": key_package()}),
  ];
  for body in refused {
    let reply = app.post(&carol, &path, body).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert_eq!(reply.error_code(), "invalid_key_package");
  }

  // Made 30 days ago: still served, but expiring within the 56-day renewal window.
  let status = upload(
    &app,
    &carol,
    &device,
    &[key_package_at(now_secs() - 30 * day, false)],
    Some(&key_package_at(now_secs() - 30 * day, true)),
  )
  .await;
  assert_eq!(
    status,
    json!({"available": 0, "expiring_soon": 1, "has_last_resort": false})
  );
  app.stop().await;
}
