//! The APNs, FCM and UnifiedPush senders against local fake push services: request format,
//! authentication, and that the payload opens with the device's push key. No database needed.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::push_fakes::{self, *};

use std::sync::Arc;
use std::time::Duration;

use axum::http::Version;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use secrecy::SecretString;
use serde_json::{Value, json};
use teamcraft_proto::{ConversationId, DeviceId, Id, PushKind, PushProvider, PushWakeUp, Snowflake, UserId};
use teamcraft_server::push::crypto::open_wakeup;
use teamcraft_server::push::{
  ApnsSender, ApnsSettings, FcmSender, FcmSettings, PushDispatcher, PushError, PushTarget, ServiceAccount,
  UnifiedPushSender, Urgency,
};

const PUSH_KEY: [u8; 32] = [7; 32];
const TTL: Duration = Duration::from_secs(3600);
const APNS_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

mod apns {
  use super::*;

  #[tokio::test]
  async fn messages_are_alerts_with_mutable_content_and_the_sealed_wake_up() {
    let (fake, dispatcher) = apns_dispatcher().await;
    dispatcher
      .deliver(
        &target(PushProvider::Apns, APNS_TOKEN),
        &wake_up(PushKind::Message),
        Urgency::High,
        TTL,
      )
      .await
      .unwrap();

    let request = &fake.requests()[0];
    assert_eq!(request.version, Version::HTTP_2);
    assert_eq!(request.path, format!("/3/device/{APNS_TOKEN}"));
    assert_eq!(request.header("apns-topic"), APNS_BUNDLE_ID);
    assert_eq!(request.header("apns-push-type"), "alert");
    assert_eq!(request.header("apns-priority"), "10");
    let expiration: u64 = request.header("apns-expiration").parse().unwrap();
    let now = jsonwebtoken::get_current_timestamp();
    assert!(
      (now + 3590..=now + 3610).contains(&expiration),
      "{expiration} vs {now}"
    );

    // Provider token: ES256, key ID in the header, team ID as issuer.
    let jwt = request.header("authorization").strip_prefix("bearer ").unwrap();
    let header = jsonwebtoken::decode_header(jwt).unwrap();
    assert_eq!(
      (header.alg, header.kid.as_deref()),
      (Algorithm::ES256, Some(APNS_KEY_ID))
    );
    let mut validation = Validation::new(Algorithm::ES256);
    validation.set_required_spec_claims(&["iss"]);
    validation.validate_exp = false;
    let claims = jsonwebtoken::decode::<Value>(
      jwt,
      &DecodingKey::from_ec_pem(APNS_PUBLIC_PEM.as_bytes()).unwrap(),
      &validation,
    )
    .unwrap()
    .claims;
    assert_eq!(claims["iss"], APNS_TEAM_ID);

    // Payload: generic alert for the notification extension, ciphertext under `w`.
    let body = request.json();
    assert_eq!(body["aps"]["mutable-content"], 1);
    assert_eq!(body["aps"]["alert"]["body"], "New message");
    let opened = open_wakeup(&PUSH_KEY, &sealed_from(body["w"].as_str().unwrap())).unwrap();
    assert_eq!(opened, wake_up(PushKind::Message));
    assert!(!String::from_utf8_lossy(&request.body).contains(&opened.conversation_id.to_string()));
  }

  #[tokio::test]
  async fn normal_messages_use_the_lower_priority() {
    let (fake, dispatcher) = apns_dispatcher().await;
    dispatcher
      .deliver(
        &target(PushProvider::Apns, APNS_TOKEN),
        &wake_up(PushKind::Message),
        Urgency::Normal,
        TTL,
      )
      .await
      .unwrap();
    assert_eq!(fake.requests()[0].header("apns-priority"), "5");
  }

  #[tokio::test]
  async fn calls_go_to_the_voip_topic_and_are_not_stored() {
    let (fake, dispatcher) = apns_dispatcher().await;
    dispatcher
      .deliver(
        &target(PushProvider::Apns, APNS_TOKEN),
        &wake_up(PushKind::Call),
        Urgency::High,
        TTL,
      )
      .await
      .unwrap();

    let request = &fake.requests()[0];
    assert_eq!(request.header("apns-topic"), format!("{APNS_BUNDLE_ID}.voip"));
    assert_eq!(request.header("apns-push-type"), "voip");
    assert_eq!(request.header("apns-priority"), "10");
    assert_eq!(request.header("apns-expiration"), "0");
    let body = request.json();
    assert!(body.get("aps").is_none(), "{body}");
    let opened = open_wakeup(&PUSH_KEY, &sealed_from(body["w"].as_str().unwrap())).unwrap();
    assert_eq!(opened.kind, PushKind::Call);
  }

  #[tokio::test]
  async fn the_provider_token_is_reused() {
    let (fake, dispatcher) = apns_dispatcher().await;
    for _ in 0..2 {
      dispatcher
        .deliver(
          &target(PushProvider::Apns, APNS_TOKEN),
          &wake_up(PushKind::Message),
          Urgency::High,
          TTL,
        )
        .await
        .unwrap();
    }
    let requests = fake.requests();
    assert_eq!(
      requests[0].header("authorization"),
      requests[1].header("authorization")
    );
  }

  #[tokio::test]
  async fn dead_tokens_are_gone_and_server_trouble_is_temporary() {
    let (fake, dispatcher) = apns_dispatcher().await;
    let send = || async {
      dispatcher
        .deliver(
          &target(PushProvider::Apns, APNS_TOKEN),
          &wake_up(PushKind::Message),
          Urgency::High,
          TTL,
        )
        .await
    };
    fake.set_reply(410, json!({"reason": "Unregistered"}));
    assert!(matches!(send().await, Err(PushError::Gone)));
    fake.set_reply(400, json!({"reason": "BadDeviceToken"}));
    assert!(matches!(send().await, Err(PushError::Gone)));
    fake.set_reply(403, json!({"reason": "InvalidProviderToken"}));
    assert!(matches!(send().await, Err(PushError::Rejected(403))));
    fake.set_reply(503, json!({"reason": "ServiceUnavailable"}));
    assert!(matches!(send().await, Err(PushError::Unavailable(_))));
    let health = dispatcher.health(&PushProvider::Apns);
    assert_eq!(health.failure_count, 2, "gone tokens are not the service's fault");
    assert!(health.is_failing());
  }

  #[tokio::test]
  async fn tokens_that_are_not_hex_never_reach_the_service() {
    let (fake, dispatcher) = apns_dispatcher().await;
    let result = dispatcher
      .deliver(
        &target(PushProvider::Apns, "../../v1/other"),
        &wake_up(PushKind::Message),
        Urgency::High,
        TTL,
      )
      .await;
    assert!(matches!(result, Err(PushError::Invalid(_))));
    assert!(fake.requests().is_empty());
  }

  #[test]
  fn a_key_that_is_not_a_p8_is_refused_at_startup() {
    let result = ApnsSender::new(ApnsSettings {
      key_pem: SecretString::from("not a key".to_owned()),
      key_id: "K".into(),
      team_id: "T".into(),
      bundle_id: "b".into(),
      base_url: "https://api.push.apple.com".into(),
      alert_text: "x".into(),
      timeout: Duration::from_secs(1),
    });
    assert!(matches!(result, Err(PushError::Invalid(_))));
  }

  async fn apns_dispatcher() -> (Fake, PushDispatcher) {
    let (fake, base_url) = push_fakes::apns().await;
    let sender = ApnsSender::new(ApnsSettings {
      key_pem: SecretString::from(APNS_KEY_PEM.to_owned()),
      key_id: APNS_KEY_ID.into(),
      team_id: APNS_TEAM_ID.into(),
      bundle_id: APNS_BUNDLE_ID.into(),
      base_url,
      alert_text: "New message".into(),
      timeout: Duration::from_secs(5),
    })
    .unwrap();
    (
      fake,
      PushDispatcher::new(4).with_sender(&PushProvider::Apns, Arc::new(sender)),
    )
  }
}

mod fcm {
  use super::*;

  #[tokio::test]
  async fn messages_are_data_only_high_priority_with_an_oauth_token_from_the_service_account() {
    let (fake, dispatcher) = fcm_dispatcher().await;
    dispatcher
      .deliver(
        &target(PushProvider::Fcm, "device-token-1"),
        &wake_up(PushKind::Message),
        Urgency::High,
        TTL,
      )
      .await
      .unwrap();

    // The OAuth exchange: a JWT bearer grant signed with the service account key.
    let token_request = &fake.token_requests()[0];
    let form = String::from_utf8(token_request.body.clone()).unwrap();
    assert!(
      form.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer"),
      "{form}"
    );
    let assertion = form.split("assertion=").nth(1).unwrap();
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[format!("{}/token", fake_base(&fake, token_request))]);
    let claims = jsonwebtoken::decode::<Value>(
      assertion,
      &DecodingKey::from_rsa_pem(FCM_PUBLIC_PEM.as_bytes()).unwrap(),
      &validation,
    )
    .unwrap()
    .claims;
    assert_eq!(claims["iss"], FCM_CLIENT_EMAIL);
    assert_eq!(
      claims["scope"],
      "https://www.googleapis.com/auth/firebase.messaging"
    );

    // The message.
    let request = &fake.requests()[0];
    assert_eq!(request.path, format!("/v1/projects/{FCM_PROJECT}/messages:send"));
    assert_eq!(
      request.header("authorization"),
      format!("Bearer {FCM_ACCESS_TOKEN}")
    );
    let message = &request.json()["message"];
    assert_eq!(message["token"], "device-token-1");
    assert_eq!(message["android"]["priority"], "HIGH");
    assert_eq!(message["android"]["ttl"], "3600s");
    assert!(message.get("notification").is_none(), "data-only: {message}");
    let opened = open_wakeup(&PUSH_KEY, &sealed_from(message["data"]["w"].as_str().unwrap())).unwrap();
    assert_eq!(opened, wake_up(PushKind::Message));
  }

  #[tokio::test]
  async fn normal_messages_use_normal_priority_and_calls_always_wake_the_phone() {
    let (fake, dispatcher) = fcm_dispatcher().await;
    for kind in [PushKind::Message, PushKind::Call] {
      dispatcher
        .deliver(
          &target(PushProvider::Fcm, "t"),
          &wake_up(kind),
          Urgency::Normal,
          TTL,
        )
        .await
        .unwrap();
    }
    let priorities: Vec<Value> = fake
      .requests()
      .iter()
      .map(|request| request.json()["message"]["android"]["priority"].clone())
      .collect();
    assert_eq!(priorities, ["NORMAL", "HIGH"]);
  }

  #[tokio::test]
  async fn the_access_token_is_requested_once_and_reused() {
    let (fake, dispatcher) = fcm_dispatcher().await;
    for _ in 0..3 {
      dispatcher
        .deliver(
          &target(PushProvider::Fcm, "t"),
          &wake_up(PushKind::Message),
          Urgency::High,
          TTL,
        )
        .await
        .unwrap();
    }
    assert_eq!(fake.token_requests().len(), 1);
    assert_eq!(fake.requests().len(), 3);
  }

  #[tokio::test]
  async fn unregistered_tokens_are_gone_and_quota_errors_are_temporary() {
    let (fake, dispatcher) = fcm_dispatcher().await;
    let send = || async {
      dispatcher
        .deliver(
          &target(PushProvider::Fcm, "t"),
          &wake_up(PushKind::Message),
          Urgency::High,
          TTL,
        )
        .await
    };
    fake.set_reply(404, json!({"error": {"status": "NOT_FOUND"}}));
    assert!(matches!(send().await, Err(PushError::Gone)));
    fake.set_reply(400, json!({"error": {"status": "INVALID_ARGUMENT"}}));
    assert!(matches!(send().await, Err(PushError::Rejected(400))));
    fake.set_reply(429, json!({"error": {"status": "RESOURCE_EXHAUSTED"}}));
    assert!(matches!(send().await, Err(PushError::Unavailable(_))));
  }

  #[tokio::test]
  async fn a_rejected_access_token_is_requested_again() {
    let (fake, dispatcher) = fcm_dispatcher().await;
    let send = || async {
      dispatcher
        .deliver(
          &target(PushProvider::Fcm, "t"),
          &wake_up(PushKind::Message),
          Urgency::High,
          TTL,
        )
        .await
    };
    send().await.unwrap();
    fake.set_reply(401, json!({"error": {"status": "UNAUTHENTICATED"}}));
    assert!(matches!(send().await, Err(PushError::Rejected(401))));
    fake.set_reply(200, Value::Null);
    send().await.unwrap();
    assert_eq!(fake.token_requests().len(), 2);
  }

  #[test]
  fn a_service_account_with_a_bad_key_is_refused_at_startup() {
    let mut account: Value = serde_json::from_str(&fcm_service_account_json("http://127.0.0.1:1")).unwrap();
    account["private_key"] = json!("not a key");
    let result = FcmSender::new(FcmSettings {
      account: ServiceAccount::from_json(&account.to_string()).unwrap(),
      base_url: "http://127.0.0.1:1".into(),
      timeout: Duration::from_secs(1),
    });
    assert!(matches!(result, Err(PushError::Invalid(_))));
  }

  /// `http://host:port` of the fake, taken from the recorded request.
  fn fake_base(_fake: &Fake, request: &Received) -> String {
    format!("http://{}", request.header("host"))
  }

  async fn fcm_dispatcher() -> (Fake, PushDispatcher) {
    let (fake, base_url) = push_fakes::fcm().await;
    let sender = FcmSender::new(FcmSettings {
      account: ServiceAccount::from_json(&fcm_service_account_json(&base_url)).unwrap(),
      base_url,
      timeout: Duration::from_secs(5),
    })
    .unwrap();
    (
      fake,
      PushDispatcher::new(4).with_sender(&PushProvider::Fcm, Arc::new(sender)),
    )
  }
}

mod unified_push {
  use super::*;

  use base64::Engine;
  use base64::engine::general_purpose::URL_SAFE_NO_PAD;
  use p256::SecretKey;
  use p256::elliptic_curve::sec1::ToEncodedPoint;
  use teamcraft_server::push::Vapid;
  use teamcraft_server::push::crypto::{VapidKey, decrypt_web_push};

  const AUTH: [u8; 16] = [5; 16];
  /// 32 bytes, base64url: a throwaway VAPID key.
  const VAPID_PRIVATE: &str = "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA";

  #[tokio::test]
  async fn the_wake_up_is_web_push_encrypted_for_the_distributor() {
    let (fake, endpoint) = push_fakes::unified_push().await;
    let ua = SecretKey::random(&mut rand_core::OsRng);
    let dispatcher = unified_dispatcher(None, true);
    dispatcher
      .deliver(
        &target(PushProvider::UnifiedPush, &subscription(&endpoint, &ua)),
        &wake_up(PushKind::Mention),
        Urgency::High,
        TTL,
      )
      .await
      .unwrap();

    let request = &fake.requests()[0];
    assert_eq!(request.path, "/up/device-1");
    assert_eq!(request.header("content-encoding"), "aes128gcm");
    assert_eq!(request.header("content-type"), "application/octet-stream");
    assert_eq!(request.header("urgency"), "high");
    assert_eq!(request.header("ttl"), "3600");
    // No VAPID key configured: no token.
    assert!(request.headers.get("authorization").is_none());
    // Only the app (holding the subscription keys) can decrypt; inside is the sealed wake-up.
    let sealed = decrypt_web_push(&ua, &AUTH, &request.body).unwrap();
    assert_eq!(
      open_wakeup(&PUSH_KEY, &sealed).unwrap(),
      wake_up(PushKind::Mention)
    );
  }

  #[tokio::test]
  async fn a_vapid_token_is_sent_when_the_server_has_a_key() {
    let (fake, endpoint) = push_fakes::unified_push().await;
    let ua = SecretKey::random(&mut rand_core::OsRng);
    let vapid = Vapid::new(
      VapidKey::from_base64url(VAPID_PRIVATE).unwrap(),
      "mailto:ops@example.com".into(),
    );
    let dispatcher = unified_dispatcher(Some(vapid), true);
    dispatcher
      .deliver(
        &target(PushProvider::UnifiedPush, &subscription(&endpoint, &ua)),
        &wake_up(PushKind::Message),
        Urgency::Normal,
        TTL,
      )
      .await
      .unwrap();
    let public_key = VapidKey::from_base64url(VAPID_PRIVATE)
      .unwrap()
      .public_key_base64url();
    let authorization = fake.requests()[0].header("authorization").to_owned();
    assert!(authorization.starts_with("vapid t="), "{authorization}");
    assert!(
      authorization.ends_with(&format!(", k={public_key}")),
      "{authorization}"
    );
  }

  #[tokio::test]
  async fn a_registration_without_keys_is_not_sent() {
    let (fake, endpoint) = push_fakes::unified_push().await;
    let dispatcher = unified_dispatcher(None, true);
    for token in [endpoint.clone(), format!(r#"{{"endpoint":"{endpoint}"}}"#)] {
      let result = dispatcher
        .deliver(
          &target(PushProvider::UnifiedPush, &token),
          &wake_up(PushKind::Message),
          Urgency::Normal,
          TTL,
        )
        .await;
      assert!(matches!(result, Err(PushError::Invalid(_))), "{token}");
    }
    assert!(fake.requests().is_empty());
  }

  #[tokio::test]
  async fn removed_endpoints_are_gone_and_failures_are_counted_apart() {
    let (fake, endpoint) = push_fakes::unified_push().await;
    let ua = SecretKey::random(&mut rand_core::OsRng);
    let token = subscription(&endpoint, &ua);
    let dispatcher = unified_dispatcher(None, true);
    let send = || async {
      dispatcher
        .deliver(
          &target(PushProvider::UnifiedPush, &token),
          &wake_up(PushKind::Message),
          Urgency::Normal,
          TTL,
        )
        .await
    };
    fake.set_reply(410, Value::Null);
    assert!(matches!(send().await, Err(PushError::Gone)));
    fake.set_reply(503, Value::Null);
    assert!(matches!(send().await, Err(PushError::Unavailable(_))));
    fake.set_reply(400, Value::Null);
    assert!(matches!(send().await, Err(PushError::Rejected(400))));
    // The endpoint is the user's: its failures never make UnifiedPush itself failing.
    let health = dispatcher.health(&PushProvider::UnifiedPush);
    assert_eq!((health.endpoint_failure_count, health.failure_count), (2, 0));
    assert!(!health.is_failing());
  }

  #[tokio::test]
  async fn endpoints_in_private_networks_are_refused_unless_development_allows_them() {
    let (fake, endpoint) = push_fakes::unified_push().await;
    let ua = SecretKey::random(&mut rand_core::OsRng);
    let dispatcher = unified_dispatcher(None, false);
    let result = dispatcher
      .deliver(
        &target(PushProvider::UnifiedPush, &subscription(&endpoint, &ua)),
        &wake_up(PushKind::Message),
        Urgency::Normal,
        TTL,
      )
      .await;
    assert!(matches!(result, Err(PushError::Invalid(_))));
    assert!(fake.requests().is_empty());
  }

  /// What the Android app registers: the distributor's endpoint with its Web Push keys.
  fn subscription(endpoint: &str, ua: &SecretKey) -> String {
    json!({
      "endpoint": endpoint,
      "keys": {
        "p256dh": URL_SAFE_NO_PAD.encode(ua.public_key().to_encoded_point(false).as_bytes()),
        "auth": URL_SAFE_NO_PAD.encode(AUTH),
      }
    })
    .to_string()
  }

  fn unified_dispatcher(maybe_vapid: Option<Vapid>, allow_private_endpoints: bool) -> PushDispatcher {
    let sender =
      UnifiedPushSender::new(maybe_vapid, Duration::from_secs(5), allow_private_endpoints).unwrap();
    PushDispatcher::new(4).with_sender(&PushProvider::UnifiedPush, Arc::new(sender))
  }
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
