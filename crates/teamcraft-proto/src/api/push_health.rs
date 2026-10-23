//! Notification diagnostics (notifications.md, "Diagnostics API"): the test push a user sends to
//! their own device, and the delivery counts admins see. Counts only: no message, conversation or
//! token ever appears here.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{DeviceId, PushProvider, UnixMillis};

wire_enum! {
  /// How the push service took a test push.
  pub enum TestPushOutcome {
    /// The push service accepted it; whether it arrives is for the device to show.
    Sent => "sent",
    /// The device has no push registration.
    NotRegistered => "not_registered",
    /// The server has no sender for the device's provider.
    NotConfigured => "not_configured",
    /// The push service is unreachable or asked to wait; try later.
    Unavailable => "unavailable",
    /// The push service refused the push (bad credentials, bad request).
    Rejected => "rejected",
    /// The registration is dead; the server forgot it and the app must register again.
    Gone => "gone",
  }
}

/// `POST /api/v1/devices/{id}/push/test`: answer of a test push to one of the caller's devices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TestPushResponse {
  pub outcome: TestPushOutcome,
  #[serde(rename = "provider", default, skip_serializing_if = "Option::is_none")]
  pub maybe_provider: Option<PushProvider>,
  /// When the server handed the push to the push service.
  pub sent_at: UnixMillis,
}

/// What the server saw of the pushes to one device since it started.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PushDeliveryCounts {
  /// Accepted by the push service (messages, mentions, calls and tests).
  pub sent: u64,
  /// Failed for good or after all retries.
  pub failed: u64,
  /// Tried again after a temporary failure.
  pub retried: u64,
  /// Silent withdrawals after the user read elsewhere.
  pub dismissed: u64,
  /// Not sent because of do not disturb.
  pub suppressed_dnd: u64,
  /// Held for an attentive desktop and dropped because the message was read in time.
  pub held_for_desktop: u64,
  #[serde(rename = "last_sent_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_sent_at: Option<UnixMillis>,
  #[serde(rename = "last_failed_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_failed_at: Option<UnixMillis>,
}

/// One device in [`PushHealthResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DevicePushHealth {
  pub device_id: DeviceId,
  /// Absent when the device has no push registration.
  #[serde(rename = "provider", default, skip_serializing_if = "Option::is_none")]
  pub maybe_provider: Option<PushProvider>,
  pub counts: PushDeliveryCounts,
}

/// `GET /api/v1/admin/users/{id}/push-health`: delivery counts per device of one user, since the
/// server process started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PushHealthResponse {
  pub devices: Vec<DevicePushHealth>,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn outcomes_have_a_stable_wire_format_and_tolerate_new_ones() {
    assert_eq!(
      serde_json::to_string(&TestPushOutcome::NotRegistered).unwrap(),
      r#""not_registered""#
    );
    let unknown: TestPushOutcome = serde_json::from_str(r#""delayed""#).unwrap();
    assert_eq!(unknown, TestPushOutcome::Unknown("delayed".into()));
  }

  #[test]
  fn empty_counts_leave_out_the_times() {
    assert_eq!(
      serde_json::to_string(&PushDeliveryCounts::default()).unwrap(),
      r#"{"sent":0,"failed":0,"retried":0,"dismissed":0,"suppressed_dnd":0,"held_for_desktop":0}"#
    );
  }
}
