//! The server's gateway client against a fake push gateway (docs/push-gateway.md): what it signs and
//! sends, registration, and how each answer maps onto the server's push errors. The real gateway is
//! tested in its own repository against the same contract fixtures. No database needed.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::fake_gateway::FakeGateway;

use std::time::Duration;

use rand_core::OsRng;
use serde_json::json;
use teamcraft_crypto::push_gateway::GatewayServerKey;
use teamcraft_proto::{
  ConversationId, DeviceId, GatewayUrgency, Id, PushKind, PushProvider, PushWakeUp, Snowflake, UserId,
};
use teamcraft_server::push::crypto::open_wakeup;
use teamcraft_server::push::{
  GatewayClient, GatewaySettings, PushDispatcher, PushError, PushTarget, Urgency,
};

const PUSH_KEY: [u8; 32] = [7; 32];
const TTL: Duration = Duration::from_secs(3600);
const APNS_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn a_push_is_signed_and_carries_the_sealed_wake_up_the_device_can_open() {
  let stack = Stack::start().await;

  deliver(
    &stack,
    PushProvider::Apns,
    APNS_TOKEN,
    PushKind::Message,
    Urgency::High,
  )
  .await
  .unwrap();

  let pushes = stack.fake.pushes();
  assert_eq!(pushes.len(), 1);
  let push = &pushes[0];
  assert_eq!(push.provider, PushProvider::Apns);
  assert_eq!(push.kind, PushKind::Message);
  assert_eq!(push.urgency, GatewayUrgency::High);
  assert_eq!(push.ttl_secs, 3600);
  assert_eq!(push.token, APNS_TOKEN);
  assert_eq!(push.request_id.as_slice().len(), 16);
  // Only the device can open it: the gateway and Apple handle ciphertext.
  let opened = open_wakeup(&PUSH_KEY, push.sealed.as_slice()).unwrap();
  assert_eq!(opened, wake_up(PushKind::Message));
}

#[tokio::test]
async fn provider_kind_and_urgency_follow_the_registration_and_the_wake_up() {
  let stack = Stack::start().await;

  deliver(
    &stack,
    PushProvider::Fcm,
    "fcm-token",
    PushKind::Call,
    Urgency::Normal,
  )
  .await
  .unwrap();
  deliver(
    &stack,
    PushProvider::Apns,
    APNS_TOKEN,
    PushKind::Mention,
    Urgency::Normal,
  )
  .await
  .unwrap();

  let pushes = stack.fake.pushes();
  assert_eq!(
    (&pushes[0].provider, &pushes[0].kind, &pushes[0].urgency),
    (&PushProvider::Fcm, &PushKind::Call, &GatewayUrgency::Normal)
  );
  assert_eq!(
    (&pushes[1].provider, &pushes[1].kind),
    (&PushProvider::Apns, &PushKind::Mention)
  );
}

#[tokio::test]
async fn every_push_has_its_own_request_id() {
  let stack = Stack::start().await;
  for _ in 0..3 {
    deliver(
      &stack,
      PushProvider::Apns,
      APNS_TOKEN,
      PushKind::Message,
      Urgency::Normal,
    )
    .await
    .unwrap();
  }
  let mut ids: Vec<_> = stack
    .fake
    .pushes()
    .iter()
    .map(|push| push.request_id.as_slice().to_vec())
    .collect();
  ids.sort();
  ids.dedup();
  assert_eq!(ids.len(), 3);
}

mod registration {
  use super::*;

  #[tokio::test]
  async fn the_first_push_registers_the_key_once() {
    let stack = Stack::start().await;
    assert_eq!(stack.fake.registrations(), 0);

    for _ in 0..2 {
      deliver(
        &stack,
        PushProvider::Apns,
        APNS_TOKEN,
        PushKind::Message,
        Urgency::Normal,
      )
      .await
      .unwrap();
    }

    assert_eq!(stack.fake.registrations(), 1);
    assert_eq!(stack.fake.pushes().len(), 2);
  }

  #[tokio::test]
  async fn a_gateway_that_forgot_the_server_learns_it_again() {
    let stack = Stack::start().await;
    deliver(
      &stack,
      PushProvider::Apns,
      APNS_TOKEN,
      PushKind::Message,
      Urgency::Normal,
    )
    .await
    .unwrap();
    stack.fake.forget_servers();

    deliver(
      &stack,
      PushProvider::Apns,
      APNS_TOKEN,
      PushKind::Message,
      Urgency::Normal,
    )
    .await
    .unwrap();

    assert_eq!(stack.fake.registrations(), 2);
    assert_eq!(stack.fake.pushes().len(), 2);
  }

  #[tokio::test]
  async fn explicit_registration_at_startup_works_and_a_server_id_is_stable() {
    let stack = Stack::start().await;
    stack.client.register().await.unwrap();
    assert_eq!(stack.fake.registrations(), 1);
    assert_eq!(stack.client.server_id().len(), 32);
  }
}

mod what_comes_back {
  use super::*;

  async fn answer(status: u16, code: &str) -> Result<(), PushError> {
    let stack = Stack::start().await;
    stack
      .fake
      .set_reply(Some((status, json!({"error_code": code, "message": code}))));
    // Registered already, so the answer is about the push.
    stack.client.register().await.unwrap();
    deliver(
      &stack,
      PushProvider::Apns,
      APNS_TOKEN,
      PushKind::Message,
      Urgency::Normal,
    )
    .await
  }

  #[tokio::test]
  async fn a_dead_token_makes_the_server_forget_the_registration() {
    assert!(matches!(answer(410, "token_gone").await, Err(PushError::Gone)));
  }

  #[tokio::test]
  async fn limits_and_outages_are_retryable_and_never_delete_devices() {
    for (status, code) in [
      (429, "rate_limited"),
      (429, "quota_exceeded"),
      (429, "token_rate_limited"),
      (502, "provider_unavailable"),
      (503, "unknown_future_code"),
    ] {
      let result = answer(status, code).await;
      assert!(
        matches!(result, Err(PushError::Unavailable(_))),
        "{code}: {result:?}"
      );
    }
  }

  #[tokio::test]
  async fn a_refusal_of_the_server_itself_is_not_held_against_its_devices() {
    for (status, code) in [
      (403, "server_blocked"),
      (401, "unauthorized"),
      (502, "provider_rejected"),
      (501, "provider_not_configured"),
    ] {
      let result = answer(status, code).await;
      assert!(
        matches!(result, Err(PushError::Rejected(rejected)) if rejected == status),
        "{code}: {result:?}"
      );
    }
  }

  #[tokio::test]
  async fn an_unreachable_gateway_is_unavailable() {
    let client = GatewayClient::new(GatewaySettings {
      url: "http://127.0.0.1:1".into(),
      key: GatewayServerKey::generate(&mut OsRng),
      timeout: Duration::from_secs(1),
    })
    .unwrap();
    let dispatcher = PushDispatcher::new(4).with_sender(
      &PushProvider::Apns,
      std::sync::Arc::new(client.sender(PushProvider::Apns)),
    );

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

pub(crate) struct Stack {
  pub(crate) fake: FakeGateway,
  pub(crate) client: std::sync::Arc<GatewayClient>,
  pub(crate) dispatcher: PushDispatcher,
}

impl Stack {
  pub(crate) async fn start() -> Self {
    let (fake, url) = FakeGateway::start().await;
    let client = GatewayClient::new(GatewaySettings {
      url,
      key: GatewayServerKey::generate(&mut OsRng),
      timeout: Duration::from_secs(5),
    })
    .unwrap();
    let dispatcher = PushDispatcher::new(4)
      .with_sender(
        &PushProvider::Apns,
        std::sync::Arc::new(client.sender(PushProvider::Apns)),
      )
      .with_sender(
        &PushProvider::Fcm,
        std::sync::Arc::new(client.sender(PushProvider::Fcm)),
      );
    Self {
      fake,
      client,
      dispatcher,
    }
  }
}

async fn deliver(
  stack: &Stack,
  provider: PushProvider,
  token: &str,
  kind: PushKind,
  urgency: Urgency,
) -> Result<(), PushError> {
  stack
    .dispatcher
    .deliver(&target(provider, token), &wake_up(kind), urgency, TTL)
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
