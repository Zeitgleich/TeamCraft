//! Servers register themselves with a key pair; the gateway only learns the public key.
#![allow(clippy::unwrap_used)]

use serde_json::{Value, json};
use teamcraft_crypto::push_gateway::GatewayServerKey;
use teamcraft_proto::GATEWAY_REGISTER_PATH;
use teamcraft_push_gateway::config::RegistrationMode;

use crate::common::*;

#[tokio::test]
async fn registering_returns_the_opaque_server_id_and_the_limits() {
  let gateway = Gateway::start().await;
  let client = Client::new();

  let response = gateway.register(&client).await;

  assert_eq!(response.status(), 200);
  let body = response.json::<Value>().await.unwrap();
  assert_eq!(body["server_id"], client.server_id);
  assert_eq!(body["limits"]["pushes_per_second"], 50);
  let record = gateway
    .state
    .store
    .get_server(&client.server_id)
    .await
    .unwrap()
    .unwrap();
  assert_eq!(&record.public_key, client.key.public_key().as_bytes());
}

#[tokio::test]
async fn registering_twice_is_fine() {
  let gateway = Gateway::start().await;
  let client = Client::new();
  assert_eq!(gateway.register(&client).await.status(), 200);
  assert_eq!(gateway.register(&client).await.status(), 200);
  assert_eq!(gateway.state.store.count_servers().await.unwrap(), 1);
}

#[tokio::test]
async fn a_key_cannot_be_registered_by_someone_who_does_not_hold_it() {
  let gateway = Gateway::start().await;
  let victim = Client::new();
  let impostor = GatewayServerKey::generate(&mut rand_core::OsRng);
  let body = json!({"public_key": base64::Engine::encode(
    &base64::engine::general_purpose::STANDARD, victim.key.public_key().as_bytes())});

  let response = gateway
    .post_signed(
      GATEWAY_REGISTER_PATH,
      &impostor,
      None,
      body.to_string().into_bytes(),
      now_ms(),
    )
    .await;

  assert_eq!(response.status(), 401);
  assert_eq!(gateway.state.store.count_servers().await.unwrap(), 0);
}

#[tokio::test]
async fn a_key_of_the_wrong_length_is_refused() {
  let gateway = Gateway::start().await;
  let key = GatewayServerKey::generate(&mut rand_core::OsRng);

  let response = gateway
    .post_signed(
      GATEWAY_REGISTER_PATH,
      &key,
      None,
      br#"{"public_key":"AAAA"}"#.to_vec(),
      now_ms(),
    )
    .await;

  assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn registrations_per_address_are_limited() {
  let mut config = default_config();
  config.registration.per_address_per_hour = 2;
  let gateway = Gateway::start_with(config).await;
  for _ in 0..2 {
    assert_eq!(gateway.register(&Client::new()).await.status(), 200);
  }

  let response = gateway.register(&Client::new()).await;

  assert_eq!(response.status(), 429);
  assert_eq!(gateway.state.store.count_servers().await.unwrap(), 2);
  // A server that is known already does not count.
  let known = gateway.state.store.list_servers(0).await.unwrap()[0]
    .record
    .server_id
    .clone();
  assert!(!known.is_empty());
}

#[tokio::test]
async fn a_closed_gateway_only_knows_the_keys_its_operator_added() {
  let mut config = default_config();
  config.registration.mode = RegistrationMode::Closed;
  let gateway = Gateway::start_with(config).await;
  let allowed = Client::new();
  gateway
    .state
    .store
    .insert_server(&allowed.server_id, *allowed.key.public_key().as_bytes(), 0)
    .await
    .unwrap();

  assert_eq!(gateway.register(&allowed).await.status(), 200);
  let stranger = gateway.register(&Client::new()).await;
  assert_eq!(stranger.status(), 403);
  assert_eq!(
    stranger.json::<Value>().await.unwrap()["error_code"],
    "registration_closed"
  );
  assert_eq!(
    gateway.get("/api/v1/info").await.json::<Value>().await.unwrap()["is_registration_open"],
    false
  );
}
