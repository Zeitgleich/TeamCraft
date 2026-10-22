//! A signed push reaches the fake APNs or FCM, and what the push service says comes back.
#![allow(clippy::unwrap_used)]

use serde_json::{Value, json};
use teamcraft_push_gateway::maintenance::flush_usage;

use crate::common::push_fakes::sealed_from;
use crate::common::*;

#[tokio::test]
async fn an_apns_alert_carries_the_sealed_wake_up_and_the_publishers_topic() {
  let gateway = Gateway::start().await;
  let client = gateway.register_client().await;

  let response = gateway
    .push(&client, &push_body("apns", "message", APNS_TOKEN))
    .await;
  assert_eq!(response.status(), 200);
  assert_eq!(response.json::<Value>().await.unwrap(), json!({"accepted": true}));

  let received = gateway.apns.requests();
  assert_eq!(received.len(), 1);
  let request = &received[0];
  assert_eq!(request.path, format!("/3/device/{APNS_TOKEN}"));
  assert_eq!(request.header("apns-topic"), push_fakes::APNS_BUNDLE_ID);
  assert_eq!(request.header("apns-push-type"), "alert");
  assert_eq!(request.header("apns-priority"), "10");
  let payload = request.json();
  assert_eq!(sealed_from(payload["w"].as_str().unwrap()), SEALED);
  // The text on the lock screen is the gateway's, not the server's.
  assert_eq!(payload["aps"]["alert"]["body"], "New message");
}

#[tokio::test]
async fn a_call_uses_the_voip_topic() {
  let gateway = Gateway::start().await;
  let client = gateway.register_client().await;

  let response = gateway
    .push(&client, &push_body("apns", "call", APNS_TOKEN))
    .await;
  assert_eq!(response.status(), 200);

  let request = &gateway.apns.requests()[0];
  assert_eq!(request.header("apns-push-type"), "voip");
  assert_eq!(
    request.header("apns-topic"),
    format!("{}.voip", push_fakes::APNS_BUNDLE_ID)
  );
  assert_eq!(sealed_from(request.json()["w"].as_str().unwrap()), SEALED);
}

#[tokio::test]
async fn an_fcm_message_is_data_only_with_the_sealed_wake_up() {
  let gateway = Gateway::start().await;
  let client = gateway.register_client().await;

  let response = gateway
    .push(&client, &push_body("fcm", "mention", FCM_TOKEN))
    .await;
  assert_eq!(response.status(), 200);

  let request = &gateway.fcm.requests()[0];
  let message = &request.json()["message"];
  assert_eq!(message["token"], FCM_TOKEN);
  assert_eq!(message["android"]["priority"], "HIGH");
  assert_eq!(sealed_from(message["data"]["w"].as_str().unwrap()), SEALED);
  assert!(message.get("notification").is_none());
}

mod invalid_tokens {
  use super::*;

  #[tokio::test]
  async fn apns_410_comes_back_as_token_gone() {
    let gateway = Gateway::start().await;
    let client = gateway.register_client().await;
    gateway.apns.set_reply(410, json!({"reason": "Unregistered"}));

    let response = gateway
      .push(&client, &push_body("apns", "message", APNS_TOKEN))
      .await;

    assert_eq!(response.status(), 410);
    assert_eq!(
      response.json::<Value>().await.unwrap()["error_code"],
      "token_gone"
    );
  }

  #[tokio::test]
  async fn apns_bad_device_token_comes_back_as_token_gone() {
    let gateway = Gateway::start().await;
    let client = gateway.register_client().await;
    gateway.apns.set_reply(400, json!({"reason": "BadDeviceToken"}));

    let response = gateway
      .push(&client, &push_body("apns", "message", APNS_TOKEN))
      .await;

    assert_eq!(response.status(), 410);
  }

  #[tokio::test]
  async fn fcm_unregistered_comes_back_as_token_gone() {
    let gateway = Gateway::start().await;
    let client = gateway.register_client().await;
    gateway
      .fcm
      .set_reply(404, json!({"error": {"status": "UNREGISTERED"}}));

    let response = gateway
      .push(&client, &push_body("fcm", "message", FCM_TOKEN))
      .await;

    assert_eq!(response.status(), 410);
    assert_eq!(
      response.json::<Value>().await.unwrap()["error_code"],
      "token_gone"
    );
  }

  #[tokio::test]
  async fn a_dead_token_is_counted_but_does_not_block_the_server() {
    let gateway = Gateway::start().await;
    let client = gateway.register_client().await;
    gateway.apns.set_reply(410, json!({"reason": "Unregistered"}));
    gateway
      .push(&client, &push_body("apns", "message", APNS_TOKEN))
      .await;
    gateway.apns.set_reply(200, Value::Null);

    let response = gateway
      .push(
        &client,
        &push_body("apns", "message", &APNS_TOKEN.replace('a', "c")),
      )
      .await;

    assert_eq!(response.status(), 200);
  }
}

mod provider_trouble {
  use super::*;

  #[tokio::test]
  async fn an_outage_of_the_push_service_is_a_retryable_bad_gateway() {
    let gateway = Gateway::start().await;
    let client = gateway.register_client().await;
    gateway
      .apns
      .set_reply(503, json!({"reason": "ServiceUnavailable"}));

    let response = gateway
      .push(&client, &push_body("apns", "message", APNS_TOKEN))
      .await;

    assert_eq!(response.status(), 502);
    assert_eq!(
      response.json::<Value>().await.unwrap()["error_code"],
      "provider_unavailable"
    );
  }

  #[tokio::test]
  async fn rejected_credentials_are_not_blamed_on_the_token() {
    let gateway = Gateway::start().await;
    let client = gateway.register_client().await;
    gateway
      .apns
      .set_reply(403, json!({"reason": "InvalidProviderToken"}));

    let response = gateway
      .push(&client, &push_body("apns", "message", APNS_TOKEN))
      .await;

    assert_eq!(response.status(), 502);
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(body["error_code"], "provider_rejected");
    // Apple's reason is the gateway operator's business, not the server's.
    assert!(!body.to_string().contains("InvalidProviderToken"));
  }

  #[tokio::test]
  async fn a_provider_without_credentials_is_not_implemented() {
    let (apns, _) = push_fakes::apns().await;
    let (fcm, _) = push_fakes::fcm().await;
    let gateway = Gateway::start_with_providers(default_config(), Default::default(), apns, fcm).await;
    let client = gateway.register_client().await;

    let response = gateway
      .push(&client, &push_body("fcm", "message", FCM_TOKEN))
      .await;

    assert_eq!(response.status(), 501);
    assert_eq!(
      response.json::<Value>().await.unwrap()["error_code"],
      "provider_not_configured"
    );
  }
}

mod validation {
  use super::*;

  #[tokio::test]
  async fn web_push_and_unified_push_never_go_through_the_gateway() {
    let gateway = Gateway::start().await;
    let client = gateway.register_client().await;
    for provider in ["webpush", "unifiedpush", "carrier-pigeon"] {
      let response = gateway
        .push(
          &client,
          &push_body(provider, "message", "https://example.com/endpoint"),
        )
        .await;
      assert_eq!(response.status(), 400, "{provider}");
    }
    assert!(gateway.apns.requests().is_empty());
  }

  #[tokio::test]
  async fn malformed_and_oversized_fields_are_refused() {
    let gateway = Gateway::start().await;
    let client = gateway.register_client().await;
    let mut cases = Vec::new();
    for (field, value) in [
      ("token", json!("")),
      ("token", json!("x".repeat(5000))),
      ("sealed", json!("")),
      ("sealed", json!("A".repeat(4000))),
      ("request_id", json!("AAAA")),
      ("kind", json!("telepathy")),
      ("urgency", json!("whenever")),
    ] {
      let mut body = push_body("apns", "message", APNS_TOKEN);
      body[field] = value;
      cases.push(body);
    }
    for body in cases {
      let status = gateway.push(&client, &body).await.status();
      assert!(status == 400 || status == 413, "{body} -> {status}");
    }
    let too_big = gateway
      .post_signed(
        "/api/v1/push",
        &client.key,
        Some(&client.server_id),
        vec![b' '; 70_000],
        now_ms(),
      )
      .await;
    assert_eq!(too_big.status(), 413);
    assert!(gateway.apns.requests().is_empty());
  }

  #[tokio::test]
  async fn the_ttl_is_capped() {
    let mut config = default_config();
    config.limits.max_ttl_secs = 120;
    let gateway = Gateway::start_with(config).await;
    let client = gateway.register_client().await;
    let mut body = push_body("apns", "message", APNS_TOKEN);
    body["kind"] = json!("message");
    body["urgency"] = json!("normal");
    body["ttl_secs"] = json!(4_000_000_000u64);

    let response = gateway.push(&client, &body).await;

    assert_eq!(response.status(), 200);
    let request = &gateway.apns.requests()[0];
    let expiration: u64 = request.header("apns-expiration").parse().unwrap();
    assert!(expiration <= now_ms() / 1000 + 121, "{expiration}");
  }
}

#[tokio::test]
async fn usage_is_counted_per_server_and_flushed_to_the_store() {
  let gateway = Gateway::start().await;
  let client = gateway.register_client().await;
  gateway
    .push(&client, &push_body("apns", "message", APNS_TOKEN))
    .await;
  gateway
    .push(&client, &push_body("fcm", "message", FCM_TOKEN))
    .await;
  gateway.apns.set_reply(410, json!({"reason": "Unregistered"}));
  gateway
    .push(
      &client,
      &push_body("apns", "message", &APNS_TOKEN.replace('a', "d")),
    )
    .await;

  flush_usage(&gateway.state).await;

  let day = i64::try_from(now_ms() / 86_400_000).unwrap();
  let usage = gateway.state.store.usage(&client.server_id, day).await.unwrap();
  assert_eq!((usage.accepted, usage.gone, usage.failed), (2, 1, 0));
}
