//! Device certificates, filtered key package claims and the `device_add` event.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_proto::GatewayEvent;
use teamcraft_server::testing::TestApp;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

fn package(text: &str) -> String {
  use base64::Engine;
  base64::engine::general_purpose::STANDARD.encode(text)
}

mod certificate {
  use super::*;

  #[tokio::test]
  async fn the_final_certificate_replaces_the_provisional_one() {
    let app = app!();
    let (carol, dave) = (member("carol"), member("dave"));
    let carol_id = user_id(&app, &carol).await;
    let device = register_device(&app, &carol).await;

    let reply = app
      .put(
        &carol,
        &format!("/devices/{device}/certificate"),
        json!({"cross_signature": "ZmluYWw="}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["cross_signature"], "ZmluYWw=");
    // Peers see the final certificate.
    let listed = app.get(&dave, &format!("/users/{carol_id}/devices")).await;
    assert_eq!(listed.body["devices"][0]["cross_signature"], "ZmluYWw=");
    app.stop().await;
  }

  #[tokio::test]
  async fn only_the_owner_can_set_it_and_it_must_be_sane() {
    let app = app!();
    let (carol, dave) = (member("carol"), member("dave"));
    let device = register_device(&app, &carol).await;
    let path = format!("/devices/{device}/certificate");

    assert_eq!(
      app
        .put(&dave, &path, json!({"cross_signature": "ZmluYWw="}))
        .await
        .error_code(),
      "device_not_found"
    );
    assert_eq!(
      app
        .put(&carol, &path, json!({"cross_signature": ""}))
        .await
        .status,
      StatusCode::BAD_REQUEST
    );
    let huge = "A".repeat(8000);
    assert_eq!(
      app
        .put(&carol, &path, json!({"cross_signature": huge}))
        .await
        .status,
      StatusCode::BAD_REQUEST
    );
    // Revoked devices can't be changed.
    app.delete(&carol, &format!("/devices/{device}")).await;
    assert_eq!(
      app
        .put(&carol, &path, json!({"cross_signature": "ZmluYWw="}))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    app.stop().await;
  }
}

mod claim_filters {
  use super::*;

  /// dave has three devices with two key packages each; carol claims in a shared channel.
  async fn setup(app: &TestApp) -> (String, String, Vec<String>) {
    let (carol, dave) = (member("carol"), member("dave"));
    let channel = channel_with(app, &[&carol, &dave]).await;
    let dave_id = user_id(app, &dave).await;
    let mut devices = Vec::new();
    for n in 0..3 {
      let device = register_device(app, &dave).await;
      let packages = vec![package(&format!("d{n}-a")), package(&format!("d{n}-b"))];
      let reply = app
        .post(
          &dave,
          &format!("/devices/{device}/key-packages"),
          json!({"key_packages": packages}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::OK);
      devices.push(device);
    }
    (channel, dave_id, devices)
  }

  async fn claim(app: &TestApp, dave_id: &str, body: serde_json::Value) -> teamcraft_server::testing::Reply {
    app
      .post(
        &member("carol"),
        &format!("/users/{dave_id}/key-packages/claim"),
        body,
      )
      .await
  }

  fn claimed_devices(reply: &teamcraft_server::testing::Reply) -> Vec<String> {
    let mut ids: Vec<String> = reply.body["packages"]
      .as_array()
      .unwrap()
      .iter()
      .map(|p| p["device_id"].as_str().unwrap().to_owned())
      .collect();
    ids.sort();
    ids
  }

  async fn left(app: &TestApp, device: &str) -> u64 {
    app
      .get(&member("dave"), &format!("/devices/{device}/key-packages"))
      .await
      .body["available"]
      .as_u64()
      .unwrap()
  }

  #[tokio::test]
  async fn without_filters_every_device_pays() {
    let app = app!();
    let (channel, dave_id, devices) = setup(&app).await;
    let reply = claim(&app, &dave_id, json!({"conversation_id": channel})).await;
    assert_eq!(claimed_devices(&reply).len(), 3);
    for device in &devices {
      assert_eq!(left(&app, device).await, 1);
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn only_the_listed_devices_are_claimed_and_the_others_keep_their_packages() {
    let app = app!();
    let (channel, dave_id, devices) = setup(&app).await;
    let reply = claim(
      &app,
      &dave_id,
      json!({"conversation_id": channel, "only_device_ids": [devices[2]]}),
    )
    .await;
    assert_eq!(claimed_devices(&reply), vec![devices[2].clone()]);
    assert_eq!(
      (
        left(&app, &devices[0]).await,
        left(&app, &devices[1]).await,
        left(&app, &devices[2]).await
      ),
      (2, 2, 1)
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn excluded_devices_are_skipped_and_the_filters_combine() {
    let app = app!();
    let (channel, dave_id, devices) = setup(&app).await;
    let reply = claim(
      &app,
      &dave_id,
      json!({"conversation_id": channel, "exclude_device_ids": [devices[0], devices[1]]}),
    )
    .await;
    assert_eq!(claimed_devices(&reply), vec![devices[2].clone()]);

    let both = claim(
      &app,
      &dave_id,
      json!({"conversation_id": channel, "only_device_ids": [devices[0], devices[1]], "exclude_device_ids": [devices[0]]}),
    )
    .await;
    assert_eq!(claimed_devices(&both), vec![devices[1].clone()]);
    assert_eq!(
      (left(&app, &devices[0]).await, left(&app, &devices[1]).await),
      (2, 1)
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn filters_that_match_nothing_answer_with_an_empty_list_not_an_error() {
    let app = app!();
    let (channel, dave_id, devices) = setup(&app).await;
    let empty = claim(
      &app,
      &dave_id,
      json!({"conversation_id": channel, "only_device_ids": [], "exclude_device_ids": []}),
    )
    .await;
    assert_eq!(
      (empty.status, empty.body["packages"].as_array().unwrap().len()),
      (StatusCode::OK, 0)
    );
    let unknown = claim(
      &app,
      &dave_id,
      json!({"conversation_id": channel, "only_device_ids": ["424242"]}),
    )
    .await;
    assert_eq!(unknown.body["packages"].as_array().unwrap().len(), 0);
    let all_excluded = claim(
      &app,
      &dave_id,
      json!({"conversation_id": channel, "exclude_device_ids": devices}),
    )
    .await;
    assert_eq!(all_excluded.body["packages"].as_array().unwrap().len(), 0);
    for device in &devices {
      assert_eq!(left(&app, device).await, 2, "nothing was burned");
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn matched_devices_without_packages_are_still_exhausted() {
    let app = app!();
    let (channel, dave_id, _) = setup(&app).await;
    let bare = register_device(&app, &member("dave")).await;
    let reply = claim(
      &app,
      &dave_id,
      json!({"conversation_id": channel, "only_device_ids": [bare]}),
    )
    .await;
    assert_eq!(
      (reply.status, reply.error_code()),
      (StatusCode::CONFLICT, "key_package_exhausted")
    );
    app.stop().await;
  }
}

mod device_add_event {
  use super::*;

  fn device_adds(events: &[GatewayEvent]) -> Vec<(String, String)> {
    events
      .iter()
      .filter_map(|event| match event {
        GatewayEvent::DeviceAdd { user_id, device_id } => Some((user_id.to_string(), device_id.to_string())),
        _ => None,
      })
      .collect()
  }

  #[tokio::test]
  async fn a_new_device_is_announced_to_people_sharing_a_conversation_and_to_the_users_other_devices() {
    let app = app!();
    let (carol, dave, erin) = (member("carol"), member("dave"), member("erin"));
    channel_with(&app, &[&carol, &dave]).await;
    let carol_id = user_id(&app, &carol).await;
    let (carol_device, dave_device, erin_device) = (
      register_device(&app, &carol).await,
      register_device(&app, &dave).await,
      register_device(&app, &erin).await,
    );
    let mut carol_socket = ws::connect(&app, &carol, &carol_device).await;
    let mut dave_socket = ws::connect(&app, &dave, &dave_device).await;
    let mut erin_socket = ws::connect(&app, &erin, &erin_device).await;

    let new_device = register_device(&app, &carol).await;
    let expected = vec![(carol_id.clone(), new_device.clone())];
    assert_eq!(
      device_adds(&ws::events(&mut dave_socket, Duration::from_millis(500)).await),
      expected
    );
    assert_eq!(
      device_adds(&ws::events(&mut carol_socket, Duration::from_millis(500)).await),
      expected
    );
    // erin shares no conversation with carol.
    assert!(device_adds(&ws::events(&mut erin_socket, Duration::from_millis(500)).await).is_empty());
    app.stop().await;
  }
}
