//! The server's gateway client against the real push gateway and local fake APNs and FCM:
//! server → gateway → push service, and what comes back (dead tokens, limits, blocks). No database
//! needed.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::push_fakes::{self, *};

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rand_core::OsRng;
use serde_json::json;
use teamcraft_crypto::push_gateway::GatewayServerKey;
use teamcraft_proto::{ConversationId, DeviceId, Id, PushKind, PushProvider, PushWakeUp, Snowflake, UserId};
use teamcraft_push_gateway::app::{AppState, api_router};
use teamcraft_push_gateway::config::GatewayConfig;
use teamcraft_push_gateway::providers::Providers;
use teamcraft_push_gateway::store::Store;
use teamcraft_push_providers::{ApnsSender, ApnsSettings, FcmSender, FcmSettings, ServiceAccount};
use teamcraft_server::push::crypto::open_wakeup;
use teamcraft_server::push::{
  GatewayClient, GatewaySettings, PushDispatcher, PushError, PushTarget, Urgency,
};

const PUSH_KEY: [u8; 32] = [7; 32];
const TTL: Duration = Duration::from_secs(3600);
const APNS_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

pub(crate) struct Stack {
  pub(crate) gateway: AppState,
  pub(crate) apns: Fake,
  pub(crate) fcm: Fake,
  pub(crate) client: Arc<GatewayClient>,
  pub(crate) dispatcher: PushDispatcher,
}

#[tokio::test]
async fn a_message_reaches_apns_through_the_gateway_with_the_sealed_wake_up_intact() {
  let stack = Stack::start(|_| {}).await;

  stack
    .dispatcher
    .deliver(
      &target(PushProvider::Apns, APNS_TOKEN),
      &wake_up(PushKind::Message),
      Urgency::High,
      TTL,
    )
    .await
    .unwrap();

  let request = &stack.apns.requests()[0];
  assert_eq!(request.path, format!("/3/device/{APNS_TOKEN}"));
  assert_eq!(request.header("apns-push-type"), "alert");
  // Only the device can open it: the gateway and Apple handled ciphertext.
  let body = request.json();
  let opened = open_wakeup(&PUSH_KEY, &sealed_from(body["w"].as_str().unwrap())).unwrap();
  assert_eq!(opened, wake_up(PushKind::Message));
}

#[tokio::test]
async fn calls_use_the_voip_topic_and_fcm_gets_a_data_message() {
  let stack = Stack::start(|_| {}).await;

  stack
    .dispatcher
    .deliver(
      &target(PushProvider::Apns, APNS_TOKEN),
      &wake_up(PushKind::Call),
      Urgency::Normal,
      TTL,
    )
    .await
    .unwrap();
  stack
    .dispatcher
    .deliver(
      &target(PushProvider::Fcm, "fcm-token"),
      &wake_up(PushKind::Mention),
      Urgency::Normal,
      TTL,
    )
    .await
    .unwrap();

  let apns = &stack.apns.requests()[0];
  assert_eq!(apns.header("apns-push-type"), "voip");
  assert_eq!(apns.header("apns-topic"), format!("{APNS_BUNDLE_ID}.voip"));
  let fcm = &stack.fcm.requests()[0].json()["message"];
  assert_eq!(fcm["token"], "fcm-token");
  let opened = open_wakeup(&PUSH_KEY, &sealed_from(fcm["data"]["w"].as_str().unwrap())).unwrap();
  assert_eq!(opened.kind, PushKind::Mention);
}

#[tokio::test]
async fn the_first_push_registers_the_server_at_the_gateway() {
  let stack = Stack::start(|_| {}).await;
  assert!(
    stack
      .gateway
      .store
      .get_server(stack.client.server_id())
      .await
      .unwrap()
      .is_none()
  );

  stack
    .dispatcher
    .deliver(
      &target(PushProvider::Apns, APNS_TOKEN),
      &wake_up(PushKind::Message),
      Urgency::Normal,
      TTL,
    )
    .await
    .unwrap();

  assert!(
    stack
      .gateway
      .store
      .get_server(stack.client.server_id())
      .await
      .unwrap()
      .is_some()
  );
}

mod what_comes_back {
  use super::*;

  #[tokio::test]
  async fn a_dead_apns_token_makes_the_server_forget_the_registration() {
    let stack = Stack::start(|_| {}).await;
    stack.apns.set_reply(410, json!({"reason": "Unregistered"}));

    let result = stack
      .dispatcher
      .deliver(
        &target(PushProvider::Apns, APNS_TOKEN),
        &wake_up(PushKind::Message),
        Urgency::Normal,
        TTL,
      )
      .await;

    assert!(matches!(result, Err(PushError::Gone)), "{result:?}");
  }

  #[tokio::test]
  async fn a_dead_fcm_token_makes_the_server_forget_the_registration() {
    let stack = Stack::start(|_| {}).await;
    stack
      .fcm
      .set_reply(404, json!({"error": {"status": "UNREGISTERED"}}));

    let result = stack
      .dispatcher
      .deliver(
        &target(PushProvider::Fcm, "fcm-token"),
        &wake_up(PushKind::Message),
        Urgency::Normal,
        TTL,
      )
      .await;

    assert!(matches!(result, Err(PushError::Gone)), "{result:?}");
  }

  #[tokio::test]
  async fn rate_limits_and_outages_are_retryable_and_never_delete_devices() {
    let stack = Stack::start(|config| {
      config.limits.burst = 1;
      config.limits.pushes_per_second = 1;
    })
    .await;
    deliver(&stack, APNS_TOKEN).await.unwrap();
    let limited = deliver(&stack, &APNS_TOKEN.replace('0', "1")).await;
    assert!(matches!(limited, Err(PushError::Unavailable(_))), "{limited:?}");

    stack.apns.set_reply(503, json!({"reason": "ServiceUnavailable"}));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let outage = deliver(&stack, &APNS_TOKEN.replace('0', "2")).await;
    assert!(matches!(outage, Err(PushError::Unavailable(_))), "{outage:?}");
  }

  #[tokio::test]
  async fn a_blocked_server_is_rejected_without_losing_its_devices() {
    let stack = Stack::start(|_| {}).await;
    deliver(&stack, APNS_TOKEN).await.unwrap();
    stack
      .gateway
      .store
      .set_blocked(stack.client.server_id(), Some("abuse"), 1)
      .await
      .unwrap();

    let result = deliver(&stack, APNS_TOKEN).await;

    assert!(matches!(result, Err(PushError::Rejected(403))), "{result:?}");
    assert_eq!(stack.apns.requests().len(), 1);
  }

  #[tokio::test]
  async fn an_unreachable_gateway_is_unavailable() {
    let client = GatewayClient::new(GatewaySettings {
      url: "http://127.0.0.1:1".into(),
      key: GatewayServerKey::generate(&mut OsRng),
      timeout: Duration::from_secs(1),
    })
    .unwrap();
    let dispatcher =
      PushDispatcher::new(4).with_token_sender(&PushProvider::Apns, client.sender(PushProvider::Apns));

    let result = dispatcher
      .deliver(
        &target(PushProvider::Apns, APNS_TOKEN),
        &wake_up(PushKind::Message),
        Urgency::Normal,
        TTL,
      )
      .await;

    assert!(matches!(result, Err(PushError::Unavailable(_))), "{result:?}");
  }
}

impl Stack {
  pub(crate) async fn start(configure: impl FnOnce(&mut GatewayConfig)) -> Self {
    let (apns, apns_url) = push_fakes::apns().await;
    let (fcm, fcm_url) = push_fakes::fcm().await;
    let timeout = Duration::from_secs(5);
    let providers = Providers {
      maybe_apns: Some(Arc::new(
        ApnsSender::new(ApnsSettings {
          key_pem: APNS_KEY_PEM.to_owned().into(),
          key_id: APNS_KEY_ID.into(),
          team_id: APNS_TEAM_ID.into(),
          bundle_id: APNS_BUNDLE_ID.into(),
          base_url: apns_url,
          alert_text: "New message".into(),
          timeout,
        })
        .unwrap(),
      )),
      maybe_fcm: Some(Arc::new(
        FcmSender::new(FcmSettings {
          account: ServiceAccount::from_json(&fcm_service_account_json(&fcm_url)).unwrap(),
          base_url: fcm_url,
          timeout,
        })
        .unwrap(),
      )),
    };
    let mut config = GatewayConfig::default();
    // Blocks and limit changes apply at once.
    config.database.server_cache_secs = 0;
    configure(&mut config);
    let gateway = AppState::new(config, Store::open(Path::new(":memory:")).unwrap(), providers);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let service = api_router(gateway.clone()).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });

    let client = GatewayClient::new(GatewaySettings {
      url,
      key: GatewayServerKey::generate(&mut OsRng),
      timeout,
    })
    .unwrap();
    let dispatcher = PushDispatcher::new(4)
      .with_token_sender(&PushProvider::Apns, client.sender(PushProvider::Apns))
      .with_token_sender(&PushProvider::Fcm, client.sender(PushProvider::Fcm));
    Self {
      gateway,
      apns,
      fcm,
      client,
      dispatcher,
    }
  }
}

async fn deliver(stack: &Stack, token: &str) -> Result<(), PushError> {
  stack
    .dispatcher
    .deliver(
      &target(PushProvider::Apns, token),
      &wake_up(PushKind::Message),
      Urgency::Normal,
      TTL,
    )
    .await
}

fn target(provider: PushProvider, token: &str) -> PushTarget {
  PushTarget {
    user: UserId::from_snowflake(Snowflake::new(1).unwrap()),
    device: DeviceId::from_snowflake(Snowflake::new(2).unwrap()),
    provider,
    token: token.to_owned(),
    push_key: PUSH_KEY.to_vec(),
  }
}

fn wake_up(kind: PushKind) -> PushWakeUp {
  PushWakeUp {
    kind,
    conversation_id: ConversationId::from_snowflake(Snowflake::new(31_337).unwrap()),
    maybe_message_id: None,
    maybe_sender_id: None,
    is_urgent: false,
  }
}
