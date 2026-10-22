//! The public info document, health and metrics.
#![allow(clippy::unwrap_used)]

use serde_json::Value;

use crate::common::*;

#[tokio::test]
async fn the_info_document_lists_providers_and_limits_without_a_signature() {
  let gateway = Gateway::start().await;

  let info = gateway.get("/api/v1/info").await.json::<Value>().await.unwrap();

  assert_eq!(info["providers"], serde_json::json!(["apns", "fcm"]));
  assert_eq!(info["is_registration_open"], true);
  assert_eq!(info["limits"]["pushes_per_token_per_minute"], 30);
}

#[tokio::test]
async fn health_answers_and_metrics_count_without_identifying_anyone() {
  let gateway = Gateway::start().await;
  let client = gateway.register_client().await;
  gateway
    .push(&client, &push_body("apns", "message", APNS_TOKEN))
    .await;
  gateway
    .push(&Client::new(), &push_body("apns", "message", APNS_TOKEN))
    .await;

  assert_eq!(gateway.get_internal("/healthz").await.status(), 200);
  let metrics = gateway.get_internal("/metrics").await.text().await.unwrap();

  assert!(
    metrics.contains("pushes_total{provider=\"apns\",result=\"accepted\"} 1"),
    "{metrics}"
  );
  assert!(
    metrics.contains("refused_total{reason=\"unknown_server\"} 1"),
    "{metrics}"
  );
  assert!(metrics.contains("registrations_total 1"), "{metrics}");
  assert!(!metrics.contains(&client.server_id));
  assert!(!metrics.contains(APNS_TOKEN));
  // The internal endpoints are not part of the public API.
  assert_eq!(gateway.get("/metrics").await.status(), 404);
}
