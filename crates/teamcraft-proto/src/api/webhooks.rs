//! Outgoing webhooks (docs/webhooks.md): the server tells external systems about events it sees
//! anyway. Metadata only — IDs, times and the names of the server's own health checks, never names
//! or content of anything people made, which the server cannot read.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::wire_enum::wire_enum;
use crate::{CallId, ConversationId, HealthCheckId, UnixMillis, UserId, WebhookId};

/// Most webhooks one instance may have.
pub const MAX_WEBHOOKS: usize = 20;
/// Longest target URL, in characters.
pub const MAX_WEBHOOK_URL_CHARS: usize = 2000;
/// Longest description, in characters.
pub const MAX_WEBHOOK_DESCRIPTION_CHARS: usize = 200;

wire_enum! {
  /// What happened. Only channels: direct and group messages are private conversations whose
  /// metadata is not passed on. Besides those, the instance's health alerts (operations.md, "Health
  /// and alerts").
  pub enum WebhookEventKind {
    /// A channel was created.
    ChannelCreated => "channel.created",
    ChannelArchived => "channel.archived",
    ChannelUnarchived => "channel.unarchived",
    /// A channel was deleted.
    ChannelDeleted => "channel.deleted",
    /// Somebody joined a channel or was added.
    MemberJoined => "member.joined",
    /// Somebody left a channel or was removed (`actor_id` differs from `subject_id` then).
    MemberLeft => "member.left",
    /// The first person joined a channel's call.
    CallStarted => "call.started",
    /// The last person left a channel's call.
    CallEnded => "call.ended",
    /// A health check failed for the hold time: admins are told (issue #213).
    HealthAlertRaised => "health.alert_raised",
    /// A health check that alerted has been fine for the clear time.
    HealthAlertResolved => "health.alert_resolved",
    /// Sent by the console's test button; no webhook subscribes to it.
    Ping => "ping",
  }
}

impl WebhookEventKind {
  /// The events a webhook can subscribe to, in the console's order.
  pub const SUBSCRIBABLE: [Self; 10] = [
    Self::ChannelCreated,
    Self::ChannelArchived,
    Self::ChannelUnarchived,
    Self::ChannelDeleted,
    Self::MemberJoined,
    Self::MemberLeft,
    Self::CallStarted,
    Self::CallEnded,
    Self::HealthAlertRaised,
    Self::HealthAlertResolved,
  ];

  pub fn is_subscribable(&self) -> bool {
    Self::SUBSCRIBABLE.contains(self)
  }
}

/// The JSON body of a webhook request. `id` is the same for every attempt of one event (the
/// receiver's idempotency key) and is also sent as the `webhook-id` header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebhookPayload {
  pub id: String,
  #[serde(rename = "type")]
  pub kind: WebhookEventKind,
  /// When the event happened on the server.
  pub created_at: UnixMillis,
  pub data: WebhookEventData,
}

/// The IDs an event concerns; which ones are set depends on the event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebhookEventData {
  #[serde(rename = "conversation_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_conversation_id: Option<ConversationId>,
  /// Who did it.
  #[serde(rename = "actor_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_actor_id: Option<UserId>,
  /// Who it was done to (the member who joined or left).
  #[serde(rename = "subject_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_subject_id: Option<UserId>,
  #[serde(rename = "call_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_call_id: Option<CallId>,
  /// The health check an alert is about: one of a fixed list, never built from input.
  #[serde(rename = "check", default, skip_serializing_if = "Option::is_none")]
  pub maybe_check: Option<HealthCheckId>,
}

/// A webhook as the console shows it. The secret is never sent back after creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminWebhook {
  pub id: WebhookId,
  pub url: String,
  #[serde(default)]
  pub description: String,
  pub events: Vec<WebhookEventKind>,
  pub is_enabled: bool,
  pub created_at: UnixMillis,
  pub health: WebhookHealth,
}

/// How deliveries to a webhook go.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebhookHealth {
  #[serde(rename = "last_success_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_success_at: Option<UnixMillis>,
  #[serde(rename = "last_failure_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_failure_at: Option<UnixMillis>,
  /// What went wrong the last time: a status code or a connection error, never a response body.
  #[serde(rename = "last_error", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_error: Option<String>,
  /// Failures in a row; deliveries wait (back off) while it is above zero.
  pub consecutive_failures: u32,
  /// Until when deliveries wait after the last failure.
  #[serde(rename = "paused_until", default, skip_serializing_if = "Option::is_none")]
  pub maybe_paused_until: Option<UnixMillis>,
  /// Events waiting for delivery.
  pub queued: u32,
  /// Events dropped because the queue was full, too old or out of attempts.
  pub dropped: u64,
}

/// `GET /api/v1/admin/webhooks`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminWebhooksResponse {
  pub webhooks: Vec<AdminWebhook>,
  /// Webhook secrets are sealed with the settings key; without one no webhook can be created.
  pub can_store_secrets: bool,
  /// Whether `[webhooks].allow_private_targets` lets URLs point into private networks.
  pub allows_private_targets: bool,
}

/// `POST /api/v1/admin/webhooks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateWebhookRequest {
  pub url: String,
  #[serde(default)]
  pub description: String,
  pub events: Vec<WebhookEventKind>,
  pub is_enabled: bool,
}

/// `PUT /api/v1/admin/webhooks/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateWebhookRequest {
  pub url: String,
  #[serde(default)]
  pub description: String,
  pub events: Vec<WebhookEventKind>,
  pub is_enabled: bool,
}

/// The answer to creating a webhook or rotating its secret: the only time the secret is shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebhookSecretResponse {
  pub webhook: AdminWebhook,
  pub secret: WebhookSecretText,
}

/// A webhook's signing secret as text (`whsec_` and base64), shown to the admin once. Not
/// printed, cleared on drop.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
pub struct WebhookSecretText(String);

impl WebhookSecretText {
  pub fn new(text: String) -> Self {
    Self(text)
  }

  pub fn expose(&self) -> &str {
    &self.0
  }
}

impl std::fmt::Debug for WebhookSecretText {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("WebhookSecretText([redacted])")
  }
}

wire_enum! {
  /// How one attempt ended.
  pub enum WebhookDeliveryOutcome {
    /// The target answered 2xx.
    Delivered => "delivered",
    /// It failed for a temporary reason and will be tried again.
    Retrying => "retrying",
    /// It failed for good: a 4xx answer, a redirect, or the attempts are used up.
    Failed => "failed",
  }
}

/// One attempt in a webhook's delivery log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebhookDeliveryEntry {
  /// The event's ID (`webhook-id`).
  pub event_id: String,
  pub event: WebhookEventKind,
  /// 1 for the first attempt.
  pub attempt: u32,
  pub outcome: WebhookDeliveryOutcome,
  #[serde(rename = "status_code", default, skip_serializing_if = "Option::is_none")]
  pub maybe_status_code: Option<u16>,
  #[serde(rename = "error", default, skip_serializing_if = "Option::is_none")]
  pub maybe_error: Option<String>,
  pub duration_ms: u32,
  pub at: UnixMillis,
}

/// `GET /api/v1/admin/webhooks/{id}/deliveries`: the latest attempts, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebhookDeliveriesResponse {
  pub entries: Vec<WebhookDeliveryEntry>,
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Id, Snowflake};

  #[test]
  fn event_kinds_have_stable_wire_names() {
    assert_eq!(
      serde_json::to_string(&WebhookEventKind::MemberJoined).unwrap(),
      "\"member.joined\""
    );
    assert_eq!(
      serde_json::from_str::<WebhookEventKind>("\"meeting.started\"").unwrap(),
      WebhookEventKind::Unknown("meeting.started".into())
    );
    assert!(!WebhookEventKind::Ping.is_subscribable());
    assert!(WebhookEventKind::CallEnded.is_subscribable());
  }

  #[test]
  fn payload_serializes_ids_as_strings_and_omits_unset_ones() {
    let payload = WebhookPayload {
      id: "42".into(),
      kind: WebhookEventKind::MemberLeft,
      created_at: UnixMillis(1_700_000_000_000),
      data: WebhookEventData {
        maybe_conversation_id: Some(ConversationId::from_snowflake(Snowflake::new(7).unwrap())),
        maybe_subject_id: Some(UserId::from_snowflake(Snowflake::new(9).unwrap())),
        ..WebhookEventData::default()
      },
    };
    assert_eq!(
      serde_json::to_value(&payload).unwrap(),
      serde_json::json!({
        "id": "42",
        "type": "member.left",
        "created_at": 1_700_000_000_000_i64,
        "data": {"conversation_id": "7", "subject_id": "9"},
      })
    );
  }

  #[test]
  fn a_health_alert_names_its_check_and_nothing_else() {
    let payload = WebhookPayload {
      id: "43".into(),
      kind: WebhookEventKind::HealthAlertRaised,
      created_at: UnixMillis(1_700_000_000_000),
      data: WebhookEventData {
        maybe_check: Some(HealthCheckId::Backup),
        ..WebhookEventData::default()
      },
    };
    assert_eq!(
      serde_json::to_value(&payload).unwrap(),
      serde_json::json!({
        "id": "43",
        "type": "health.alert_raised",
        "created_at": 1_700_000_000_000_i64,
        "data": {"check": "backup"},
      })
    );
    assert!(WebhookEventKind::HealthAlertResolved.is_subscribable());
  }

  #[test]
  fn the_secret_is_not_printed() {
    let secret = WebhookSecretText::new("whsec_abc".into());
    assert!(!format!("{secret:?}").contains("abc"));
  }
}
