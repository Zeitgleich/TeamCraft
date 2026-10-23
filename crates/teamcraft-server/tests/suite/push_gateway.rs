//! The server's gateway client against a fake push gateway (docs/push-gateway.md): what it signs and
//! sends, registration, and how each answer maps onto the server's push errors. The real gateway is
//! tested in its own repository against the same contract fixtures. No database needed.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::fake_gateway::FakeGateway;

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::OsRng;
use serde_json::json;
use teamcraft_crypto::push_gateway::{GatewayServerKey, seal_bound_token};
use teamcraft_proto::{
  BOUND_TOKEN_PREFIX, ConversationId, DeviceId, GatewayUrgency, Id, PushKind, PushProvider, PushWakeUp,
  Snowflake, UserId,
};
use teamcraft_server::push::crypto::open_wakeup;
use teamcraft_server::push::{
  GatewayBinding, GatewayClient, GatewaySettings, PushDispatcher, PushError, PushTarget, Urgency,
};

const PUSH_KEY: [u8; 32] = [7; 32];
const TTL: Duration = Duration::from_secs(3600);
const APNS_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn a_push_is_signed_and_carries_the_sealed_token_and_the_wake_up_the_device_can_open() {
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
  let request = &push.request;
  assert_eq!(request.provider, PushProvider::Apns);
  assert_eq!(request.kind, PushKind::Message);
  assert_eq!(request.urgency, GatewayUrgency::High);
  assert_eq!(request.ttl_secs, 3600);
  assert_eq!(request.request_id.as_slice().len(), 16);
  // The gateway got the app's sealed blob, not a raw token; opened, it names this server.
  assert!(request.maybe_token.is_none() && request.maybe_bound_token.is_some());
  let bound = push.maybe_bound.as_ref().unwrap();
  assert_eq!(bound.token, APNS_TOKEN);
  assert_eq!(bound.provider, "apns");
  assert_eq!(bound.server_id, stack.client.server_id());
  // Only the device can open the wake-up: the gateway and Apple handle ciphertext.
  let opened = open_wakeup(&PUSH_KEY, request.sealed.as_slice()).unwrap();
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
  let (first, second) = (&pushes[0].request, &pushes[1].request);
  assert_eq!(
    (&first.provider, &first.kind, &first.urgency),
    (&PushProvider::Fcm, &PushKind::Call, &GatewayUrgency::Normal)
  );
  assert_eq!(
    (&second.provider, &second.kind),
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
    .map(|push| push.request.request_id.as_slice().to_vec())
    .collect();
  ids.sort();
  ids.dedup();
  assert_eq!(ids.len(), 3);
}

mod bound_tokens {
  use super::*;

  #[tokio::test]
  async fn a_registration_with_a_raw_token_is_dead_weight_and_never_reaches_the_gateway() {
    let stack = Stack::start().await;

    let result = stack
      .dispatcher
      .deliver(
        &target(PushProvider::Apns, APNS_TOKEN),
        &wake_up(PushKind::Message),
        Urgency::Normal,
        TTL,
      )
      .await;

    // The server forgets it; the app registers again, sealed.
    assert!(matches!(result, Err(PushError::Gone)), "{result:?}");
    assert!(stack.fake.pushes().is_empty());
  }

  #[tokio::test]
  async fn raw_tokens_go_out_as_they_are_only_while_the_transition_setting_is_on() {
    let stack = Stack::start_with(true).await;
    stack.fake.accept_unbound();

    let result = stack
      .dispatcher
      .deliver(
        &target(PushProvider::Apns, APNS_TOKEN),
        &wake_up(PushKind::Message),
        Urgency::Normal,
        TTL,
      )
      .await;

    result.unwrap();
    let push = &stack.fake.pushes()[0];
    assert_eq!(push.request.maybe_token.as_deref(), Some(APNS_TOKEN));
    assert!(push.request.maybe_bound_token.is_none() && push.maybe_bound.is_none());
  }

  #[tokio::test]
  async fn junk_that_only_looks_sealed_is_invalid() {
    let stack = Stack::start().await;
    for token in ["tcgw1:", "tcgw1:***not base64***", "tcgw1:AAAA"] {
      let result = stack
        .dispatcher
        .deliver(
          &target(PushProvider::Apns, token),
          &wake_up(PushKind::Message),
          Urgency::Normal,
          TTL,
        )
        .await;
      assert!(
        matches!(result, Err(PushError::Invalid(_))) || matches!(result, Err(PushError::Gone)),
        "{token}: {result:?}"
      );
    }
  }

  #[tokio::test]
  async fn a_token_sealed_for_another_server_expired_or_revoked_makes_the_server_forget_it() {
    let stack = Stack::start().await;
    let now = now_ms();
    let other_server = seal(
      &stack,
      "apns",
      APNS_TOKEN,
      "00000000000000000000000000000000",
      now + 60_000,
    );
    let expired = seal(&stack, "apns", APNS_TOKEN, stack.client.server_id(), now - 1_000);
    for token in [other_server, expired] {
      let result = stack
        .dispatcher
        .deliver(
          &target(PushProvider::Apns, &token),
          &wake_up(PushKind::Message),
          Urgency::Normal,
          TTL,
        )
        .await;
      assert!(matches!(result, Err(PushError::Gone)), "{result:?}");
    }
    assert!(stack.fake.pushes().is_empty());
  }
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
  async fn a_bare_410_deletes_nothing() {
    assert!(!matches!(
      answer(410, "unknown_future_code").await,
      Err(PushError::Gone)
    ));
  }

  #[tokio::test]
  async fn a_problem_of_one_token_is_not_the_gateways() {
    for (status, code) in [(429, "token_rate_limited"), (400, "invalid_request")] {
      let result = answer(status, code).await;
      assert!(matches!(result, Err(PushError::Invalid(_))), "{code}: {result:?}");
    }
  }

  #[tokio::test]
  async fn a_server_over_its_limit_waits_as_told_before_the_next_push() {
    let stack = Stack::start().await;
    stack.client.register().await.unwrap();
    stack.fake.set_reply(Some((
      429,
      json!({"error_code": "quota_exceeded", "message": "quota", "retry_after_secs": 3600}),
    )));
    let push = || {
      deliver(
        &stack,
        PushProvider::Apns,
        APNS_TOKEN,
        PushKind::Message,
        Urgency::Normal,
      )
    };
    assert!(matches!(push().await, Err(PushError::Unavailable { .. })));
    stack.fake.set_reply(None);
    assert!(matches!(push().await, Err(PushError::Unavailable { .. })));
    assert_eq!(
      stack.fake.push_attempts(),
      1,
      "the second push waited instead of going out"
    );
  }

  #[tokio::test]
  async fn limits_and_outages_are_retryable_and_never_delete_devices() {
    for (status, code) in [
      (429, "rate_limited"),
      (429, "quota_exceeded"),
      (502, "provider_unavailable"),
      (503, "unknown_future_code"),
    ] {
      let result = answer(status, code).await;
      assert!(
        matches!(result, Err(PushError::Unavailable { .. })),
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
      accept_unbound_tokens: false,
    })
    .unwrap();
    let dispatcher = PushDispatcher::new(4).with_sender(
      &PushProvider::Apns,
      std::sync::Arc::new(client.sender(PushProvider::Apns)),
    );

    let result = dispatcher
      .deliver(
        // Sealed or not, the gateway can't be reached.
        &target(PushProvider::Apns, "tcgw1:AAAAAAAAAAAAAAAAAAAAAAAA"),
        &wake_up(PushKind::Message),
        Urgency::Normal,
        TTL,
      )
      .await;

    assert!(matches!(result, Err(PushError::Unavailable { .. })), "{result:?}");
  }
}

pub(crate) struct Stack {
  pub(crate) fake: FakeGateway,
  pub(crate) client: std::sync::Arc<GatewayClient>,
  pub(crate) dispatcher: PushDispatcher,
}

impl Stack {
  /// A device token as the app registers it: sealed for the gateway, bound to this server.
  pub(crate) fn sealed_token(&self, provider: &str, raw: &str) -> String {
    seal(self, provider, raw, self.client.server_id(), now_ms() + 3_600_000)
  }
}

fn seal(stack: &Stack, provider: &str, raw: &str, server_id: &str, expires_at_ms: u64) -> String {
  let blob = seal_bound_token(
    &stack.fake.token_public_key(),
    provider,
    server_id,
    expires_at_ms,
    raw,
  )
  .unwrap();
  format!("{BOUND_TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(blob))
}

fn now_ms() -> u64 {
  u64::try_from(
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap()
      .as_millis(),
  )
  .unwrap()
}

impl Stack {
  pub(crate) async fn start() -> Self {
    Self::start_with(false).await
  }

  pub(crate) async fn start_with(accept_unbound_tokens: bool) -> Self {
    let (fake, url) = FakeGateway::start().await;
    let client = GatewayClient::new(GatewaySettings {
      url,
      key: GatewayServerKey::generate(&mut OsRng),
      timeout: Duration::from_secs(5),
      accept_unbound_tokens,
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
      )
      .with_gateway_binding(GatewayBinding {
        url: client.url().to_owned(),
        server_id: client.server_id().to_owned(),
        providers: vec![PushProvider::Apns, PushProvider::Fcm],
      });
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
  let provider_name = provider.as_str().to_owned();
  let sealed = stack.sealed_token(&provider_name, token);
  stack
    .dispatcher
    .deliver(&target(provider, &sealed), &wake_up(kind), urgency, TTL)
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
    maybe_thread_id: None,
    maybe_call_id: None,
    maybe_badge: None,
    maybe_sent_at: None,
  }
}
