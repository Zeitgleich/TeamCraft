//! Wire types of the push gateway API (docs/push-gateway.md; the contract lives in the gateway
//! repository, docs/api.md, and is pinned by tests/fixtures/push_gateway/). A server talks to a gateway with
//! these; the gateway sees device tokens and sealed wake-ups, never users, devices or keys.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{Base64Bytes, PushKind, PushProvider};

/// Hex server ID the signature belongs to (`GatewayPublicKey::server_id`).
pub const GATEWAY_SERVER_ID_HEADER: &str = "x-teamcraft-server";
/// Sender clock in milliseconds since the Unix epoch.
pub const GATEWAY_TIMESTAMP_HEADER: &str = "x-teamcraft-timestamp";
/// Base64url (no padding) Ed25519 signature over the request (`teamcraft_crypto::push_gateway`).
pub const GATEWAY_SIGNATURE_HEADER: &str = "x-teamcraft-signature";

pub const GATEWAY_PUSH_PATH: &str = "/v1/push";
pub const GATEWAY_REGISTER_PATH: &str = "/v1/servers";
pub const GATEWAY_INFO_PATH: &str = "/v1/info";

/// Length of [`GatewayPushRequest::request_id`].
pub const GATEWAY_REQUEST_ID_LEN: usize = 16;

wire_enum! {
  /// How eagerly the push service delivers (APNs priority, FCM priority).
  pub enum GatewayUrgency {
    Normal => "normal",
    High => "high",
  }
}

/// `POST /api/v1/servers`, signed with the key being registered. Idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GatewayRegisterRequest {
  /// The server's Ed25519 public key (32 bytes).
  pub public_key: Base64Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GatewayRegisterResponse {
  /// The opaque ID of the server at this gateway.
  pub server_id: String,
  pub limits: GatewayLimits,
}

/// The limits that apply to one server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GatewayLimits {
  pub pushes_per_second: u32,
  pub burst: u32,
  pub pushes_per_day: u64,
  /// Pushes per device token and minute.
  pub pushes_per_token_per_minute: u32,
}

/// `POST /api/v1/push`, signed. One sealed wake-up for one device token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GatewayPushRequest {
  /// 16 random bytes; the gateway refuses a request ID it saw within the timestamp window.
  pub request_id: Base64Bytes,
  /// `apns` or `fcm`. Web Push and UnifiedPush never go through a gateway.
  pub provider: PushProvider,
  /// What the wake-up is for; picks the APNs channel (`call` uses the VoIP topic).
  pub kind: PushKind,
  pub urgency: GatewayUrgency,
  /// How long the push service may hold the wake-up for an offline device.
  pub ttl_secs: u32,
  /// The APNs device token (hex) or FCM registration token.
  pub token: String,
  /// The wake-up sealed to the device's push key; opaque to the gateway.
  pub sealed: Base64Bytes,
}

// The token identifies a device at Apple or Google: keep it out of debug output.
impl std::fmt::Debug for GatewayPushRequest {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("GatewayPushRequest")
      .field("provider", &self.provider)
      .field("kind", &self.kind)
      .field("token", &"[redacted]")
      .finish_non_exhaustive()
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GatewayPushResponse {
  /// The push service accepted the wake-up.
  pub accepted: bool,
}

/// `GET /api/v1/info`, unauthenticated: what a server needs to decide whether to use a gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GatewayInfoResponse {
  pub version: String,
  /// Providers with credentials at this gateway.
  pub providers: Vec<PushProvider>,
  /// Whether unknown servers may register themselves.
  pub is_registration_open: bool,
  pub limits: GatewayLimits,
}

wire_enum! {
  /// Stable error identifiers of the gateway API.
  pub enum GatewayErrorCode {
    InvalidRequest => "invalid_request",
    /// Missing, wrong or stale signature.
    Unauthorized => "unauthorized",
    UnknownServer => "unknown_server",
    ServerBlocked => "server_blocked",
    ReplayedRequest => "replayed_request",
    RegistrationClosed => "registration_closed",
    PayloadTooLarge => "payload_too_large",
    /// Per-second rate of the server exceeded; retry later.
    RateLimited => "rate_limited",
    /// The server's daily quota is used up.
    QuotaExceeded => "quota_exceeded",
    /// This device token receives too many pushes.
    TokenRateLimited => "token_rate_limited",
    /// APNs or FCM says the token is dead (410): the server forgets the registration.
    TokenGone => "token_gone",
    ProviderNotConfigured => "provider_not_configured",
    /// The gateway's credentials were rejected: the gateway operator's problem, not the token's.
    ProviderRejected => "provider_rejected",
    ProviderUnavailable => "provider_unavailable",
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GatewayErrorBody {
  pub error_code: GatewayErrorCode,
  pub message: String,
  #[serde(rename = "retry_after_secs", default, skip_serializing_if = "Option::is_none")]
  pub maybe_retry_after_secs: Option<u32>,
}

#[cfg(test)]
mod tests {
  use serde::de::DeserializeOwned;
  use serde_json::Value;

  use super::*;

  // The contract fixtures (docs/api.md in the gateway repository, identical copies in both
  // repositories): what the gateway accepts and answers, byte for byte.
  const PUSH_REQUEST: &str = include_str!("../../tests/fixtures/push_gateway/push_request.json");

  #[test]
  fn the_contract_fixtures_parse_and_serialize_back_unchanged() {
    fn check<T: Serialize + DeserializeOwned>(fixture: &str) -> T {
      let parsed: T = serde_json::from_str(fixture).unwrap();
      let expected: Value = serde_json::from_str(fixture).unwrap();
      assert_eq!(serde_json::to_value(&parsed).unwrap(), expected, "{fixture}");
      parsed
    }

    let request: GatewayPushRequest = check(PUSH_REQUEST);
    assert_eq!(request.provider, PushProvider::Apns);
    assert_eq!(request.kind, PushKind::Message);
    assert_eq!(request.urgency, GatewayUrgency::High);
    assert_eq!(request.ttl_secs, 3600);
    assert_eq!(request.request_id.as_slice(), [1u8; GATEWAY_REQUEST_ID_LEN]);
    assert_eq!(request.sealed.as_slice(), b"\x01hello");
    let _: GatewayPushResponse = check(include_str!(
      "../../tests/fixtures/push_gateway/push_response.json"
    ));
    let register: GatewayRegisterRequest = check(include_str!(
      "../../tests/fixtures/push_gateway/register_request.json"
    ));
    assert_eq!(register.public_key.as_slice().len(), 32);
    let response: GatewayRegisterResponse = check(include_str!(
      "../../tests/fixtures/push_gateway/register_response.json"
    ));
    assert_eq!(response.limits.burst, 200);
    let info: GatewayInfoResponse = check(include_str!(
      "../../tests/fixtures/push_gateway/info_response.json"
    ));
    assert_eq!(info.providers, [PushProvider::Apns, PushProvider::Fcm]);
    let limited: GatewayErrorBody = check(include_str!(
      "../../tests/fixtures/push_gateway/error_rate_limited.json"
    ));
    assert_eq!(limited.error_code, GatewayErrorCode::RateLimited);
    assert_eq!(limited.maybe_retry_after_secs, Some(2));
    let gone: GatewayErrorBody = check(include_str!(
      "../../tests/fixtures/push_gateway/error_token_gone.json"
    ));
    assert_eq!(gone.error_code, GatewayErrorCode::TokenGone);
  }

  #[test]
  fn error_codes_are_stable_and_unknown_ones_survive() {
    let body = r#"{"error_code":"token_gone","message":"x"}"#;
    let parsed: GatewayErrorBody = serde_json::from_str(body).unwrap();
    assert_eq!(parsed.error_code, GatewayErrorCode::TokenGone);
    assert_eq!(serde_json::to_string(&parsed).unwrap(), body);
    let future: GatewayErrorBody =
      serde_json::from_str(r#"{"error_code":"from_the_future","message":"x"}"#).unwrap();
    assert_eq!(
      future.error_code,
      GatewayErrorCode::Unknown("from_the_future".into())
    );
  }

  #[test]
  fn a_push_request_has_a_fixed_wire_form_and_hides_the_token() {
    let request = GatewayPushRequest {
      request_id: Base64Bytes::new(vec![1; 16]),
      provider: PushProvider::Apns,
      kind: PushKind::Call,
      urgency: GatewayUrgency::High,
      ttl_secs: 30,
      token: "secret-token".into(),
      sealed: Base64Bytes::new(vec![2, 3]),
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["provider"], "apns");
    assert_eq!(json["kind"], "call");
    assert_eq!(json["urgency"], "high");
    assert_eq!(json["ttl_secs"], 30);
    assert!(!format!("{request:?}").contains("secret-token"));
  }
}
