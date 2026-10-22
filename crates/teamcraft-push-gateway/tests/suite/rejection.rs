//! Requests the gateway must refuse: bad signatures, stale or replayed requests, blocked servers
//! and every limit.
#![allow(clippy::unwrap_used)]

use serde_json::Value;
use teamcraft_crypto::push_gateway::GatewayServerKey;
use teamcraft_proto::{GATEWAY_PUSH_PATH, GATEWAY_SERVER_ID_HEADER};

use crate::common::*;

async fn code(response: reqwest::Response) -> (u16, String) {
  let status = response.status().as_u16();
  let body: Value = response.json().await.unwrap();
  (status, body["error_code"].as_str().unwrap_or_default().to_owned())
}

mod signatures {
  use super::*;

  #[tokio::test]
  async fn a_request_signed_with_another_key_is_refused() {
    let gateway = Gateway::start().await;
    let client = gateway.register_client().await;
    let impostor = GatewayServerKey::generate(&mut rand_core::OsRng);
    let body = push_body("apns", "message", APNS_TOKEN).to_string().into_bytes();

    let response = gateway
      .post_signed(
        GATEWAY_PUSH_PATH,
        &impostor,
        Some(&client.server_id),
        body,
        now_ms(),
      )
      .await;

    assert_eq!(code(response).await, (401, "unauthorized".into()));
    assert!(gateway.apns.requests().is_empty());
  }

  #[tokio::test]
  async fn a_body_changed_after_signing_is_refused() {
    let gateway = Gateway::start().await;
    let client = gateway.register_client().await;
    let body = push_body("apns", "message", APNS_TOKEN).to_string();
    let timestamp = now_ms();
    let signature = client.key.sign(&teamcraft_crypto::push_gateway::GatewayRequest {
      method: "POST",
      path: GATEWAY_PUSH_PATH,
      timestamp_ms: timestamp,
      body: body.as_bytes(),
    });

    let tampered = body.replace("message", "mention");
    let response = reqwest::Client::new()
      .post(format!("{}{GATEWAY_PUSH_PATH}", gateway.url))
      .header(GATEWAY_SERVER_ID_HEADER, &client.server_id)
      .header("x-teamcraft-timestamp", timestamp.to_string())
      .header(
        "x-teamcraft-signature",
        base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, signature),
      )
      .body(tampered)
      .send()
      .await
      .unwrap();

    assert_eq!(code(response).await, (401, "unauthorized".into()));
  }

  #[tokio::test]
  async fn missing_headers_are_refused() {
    let gateway = Gateway::start().await;
    let response = reqwest::Client::new()
      .post(format!("{}{GATEWAY_PUSH_PATH}", gateway.url))
      .body("{}")
      .send()
      .await
      .unwrap();
    assert_eq!(code(response).await, (401, "unauthorized".into()));
  }

  #[tokio::test]
  async fn stale_and_future_timestamps_are_refused() {
    let gateway = Gateway::start().await;
    let client = gateway.register_client().await;
    for timestamp in [now_ms() - 10 * 60_000, now_ms() + 10 * 60_000] {
      let body = push_body("apns", "message", APNS_TOKEN).to_string().into_bytes();
      let response = gateway
        .post_signed(
          GATEWAY_PUSH_PATH,
          &client.key,
          Some(&client.server_id),
          body,
          timestamp,
        )
        .await;
      assert_eq!(code(response).await, (401, "unauthorized".into()));
    }
    assert!(gateway.apns.requests().is_empty());
  }

  #[tokio::test]
  async fn an_unknown_server_is_told_to_register() {
    let gateway = Gateway::start().await;
    let stranger = Client::new();

    let response = gateway
      .push(&stranger, &push_body("apns", "message", APNS_TOKEN))
      .await;

    assert_eq!(code(response).await, (401, "unknown_server".into()));
  }

  #[tokio::test]
  async fn a_request_cannot_be_replayed() {
    let gateway = Gateway::start().await;
    let client = gateway.register_client().await;
    let body = push_body("apns", "message", APNS_TOKEN);

    assert_eq!(gateway.push(&client, &body).await.status(), 200);
    let replay = gateway.push(&client, &body).await;

    assert_eq!(code(replay).await, (409, "replayed_request".into()));
    assert_eq!(gateway.apns.requests().len(), 1);
  }
}

mod block_list {
  use super::*;

  #[tokio::test]
  async fn a_blocked_server_is_refused_and_can_be_unblocked() {
    let gateway = Gateway::start().await;
    let client = gateway.register_client().await;
    gateway
      .state
      .store
      .set_blocked(&client.server_id, Some("abuse"), 1)
      .await
      .unwrap();

    let response = gateway
      .push(&client, &push_body("apns", "message", APNS_TOKEN))
      .await;
    assert_eq!(code(response).await, (403, "server_blocked".into()));
    assert!(gateway.apns.requests().is_empty());
    // It cannot register its way back in either.
    assert_eq!(
      code(gateway.register(&client).await).await,
      (403, "server_blocked".into())
    );

    gateway
      .state
      .store
      .set_blocked(&client.server_id, None, 2)
      .await
      .unwrap();
    let response = gateway
      .push(&client, &push_body("apns", "message", APNS_TOKEN))
      .await;
    assert_eq!(response.status(), 200);
  }

  #[tokio::test]
  async fn blocking_one_server_leaves_the_others_alone() {
    let gateway = Gateway::start().await;
    let (bad, good) = (gateway.register_client().await, gateway.register_client().await);
    gateway
      .state
      .store
      .set_blocked(&bad.server_id, Some("abuse"), 1)
      .await
      .unwrap();

    let response = gateway.push(&good, &push_body("fcm", "message", FCM_TOKEN)).await;

    assert_eq!(response.status(), 200);
  }
}

mod limits {
  use super::*;

  fn config(
    per_second: u32,
    burst: u32,
    per_day: u64,
    per_token: u32,
  ) -> teamcraft_push_gateway::config::GatewayConfig {
    let mut config = default_config();
    config.limits.pushes_per_second = per_second;
    config.limits.burst = burst;
    config.limits.pushes_per_day = per_day;
    config.limits.pushes_per_token_per_minute = per_token;
    config
  }

  #[tokio::test]
  async fn a_server_over_its_rate_waits() {
    let gateway = Gateway::start_with(config(1, 2, 1000, 1000)).await;
    let client = gateway.register_client().await;
    for _ in 0..2 {
      assert_eq!(
        gateway
          .push(&client, &push_body("apns", "message", APNS_TOKEN))
          .await
          .status(),
        200
      );
    }

    let response = gateway
      .push(&client, &push_body("apns", "message", APNS_TOKEN))
      .await;

    assert_eq!(response.status(), 429);
    assert!(response.headers().contains_key("retry-after"));
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(body["error_code"], "rate_limited");
    assert!(body["retry_after_secs"].as_u64().unwrap() >= 1);
    assert_eq!(gateway.apns.requests().len(), 2);
  }

  #[tokio::test]
  async fn one_noisy_server_does_not_slow_another() {
    let gateway = Gateway::start_with(config(1, 1, 1000, 1000)).await;
    let (noisy, quiet) = (gateway.register_client().await, gateway.register_client().await);
    gateway
      .push(&noisy, &push_body("apns", "message", APNS_TOKEN))
      .await;
    assert_eq!(
      gateway
        .push(&noisy, &push_body("apns", "message", APNS_TOKEN))
        .await
        .status(),
      429
    );

    assert_eq!(
      gateway
        .push(&quiet, &push_body("apns", "message", APNS_TOKEN))
        .await
        .status(),
      200
    );
  }

  #[tokio::test]
  async fn the_daily_quota_ends_the_day_for_that_server() {
    let gateway = Gateway::start_with(config(1000, 1000, 2, 1000)).await;
    let client = gateway.register_client().await;
    for _ in 0..2 {
      assert_eq!(
        gateway
          .push(&client, &push_body("apns", "message", APNS_TOKEN))
          .await
          .status(),
        200
      );
    }

    let response = gateway
      .push(&client, &push_body("apns", "message", APNS_TOKEN))
      .await;

    assert_eq!(code(response).await, (429, "quota_exceeded".into()));
  }

  #[tokio::test]
  async fn a_per_server_override_replaces_the_default() {
    let gateway = Gateway::start_with(config(1000, 1000, 1000, 1000)).await;
    let client = gateway.register_client().await;
    let limits = teamcraft_push_gateway::store::LimitOverrides {
      maybe_pushes_per_day: Some(1),
      ..Default::default()
    };
    gateway
      .state
      .store
      .set_limits(&client.server_id, limits)
      .await
      .unwrap();
    assert_eq!(
      gateway
        .push(&client, &push_body("apns", "message", APNS_TOKEN))
        .await
        .status(),
      200
    );

    let response = gateway
      .push(&client, &push_body("apns", "message", APNS_TOKEN))
      .await;

    assert_eq!(code(response).await, (429, "quota_exceeded".into()));
  }

  #[tokio::test]
  async fn one_device_token_cannot_be_flooded_even_by_a_server_within_its_rate() {
    let gateway = Gateway::start_with(config(1000, 1000, 1000, 2)).await;
    let client = gateway.register_client().await;
    for _ in 0..2 {
      assert_eq!(
        gateway
          .push(&client, &push_body("apns", "message", APNS_TOKEN))
          .await
          .status(),
        200
      );
    }

    let response = gateway
      .push(&client, &push_body("apns", "message", APNS_TOKEN))
      .await;
    assert_eq!(code(response).await, (429, "token_rate_limited".into()));
    // Other devices of the same server are fine.
    let other = APNS_TOKEN.replace('a', "e");
    assert_eq!(
      gateway
        .push(&client, &push_body("apns", "message", &other))
        .await
        .status(),
      200
    );
  }

  #[tokio::test]
  async fn the_same_token_through_two_servers_shares_the_token_limit() {
    let gateway = Gateway::start_with(config(1000, 1000, 1000, 1)).await;
    let (first, second) = (gateway.register_client().await, gateway.register_client().await);
    assert_eq!(
      gateway
        .push(&first, &push_body("apns", "message", APNS_TOKEN))
        .await
        .status(),
      200
    );

    let response = gateway
      .push(&second, &push_body("apns", "message", APNS_TOKEN))
      .await;

    assert_eq!(code(response).await, (429, "token_rate_limited".into()));
  }
}
