//! Announcing a device link to the user's other devices (e2ee.md §5, "Approve prompt").

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::Base64Bytes;

/// Body of `POST /api/v1/device-links`: a new device of the caller that waits at `rendezvous_id`
/// (it joined the relay already) asks the caller's other devices to approve it. The server relays
/// this to the devices and sees the authentication key, as it does in the approve-prompt variant of
/// the design: the verification code the user types on the approving device, derived from the
/// handshake, is what detects a server that injects itself.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateDeviceLinkRequest {
  /// The relay ID the new device joined (32 hex characters).
  pub rendezvous_id: String,
  /// The handshake's authentication key (`AK`, 32 bytes).
  pub key: Base64Bytes,
  /// The name the user gave the new device; shown in the prompt, never trusted for anything else.
  pub device_name: String,
  /// What kind of device it is; shown in the prompt.
  pub platform: DeviceLinkPlatform,
}

/// A pending request as the caller's devices see it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeviceLinkRequest {
  /// The relay ID; names the request in every other call.
  pub id: String,
  pub key: Base64Bytes,
  pub device_name: String,
  pub platform: DeviceLinkPlatform,
  /// How long the request stays open from the moment the server sent it. Relative, so a wrong
  /// clock on the device doesn't keep a stale prompt around or hide a fresh one.
  pub expires_in_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListDeviceLinksResponse {
  pub requests: Vec<DeviceLinkRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceLinkPlatform {
  Android,
  Ios,
  Macos,
  Windows,
  Linux,
  Web,
  #[serde(other)]
  Other,
}

/// Why a request is gone (gateway event `device_link_closed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceLinkClosedReason {
  /// A device took it up (joined the relay as the approving side); the others drop their prompt.
  Claimed,
  /// A device of the user denied it.
  Rejected,
  /// The new device gave up.
  Withdrawn,
  /// Its time ran out.
  Expired,
  #[serde(other)]
  Unknown,
}

impl std::fmt::Debug for CreateDeviceLinkRequest {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("CreateDeviceLinkRequest")
      .field("rendezvous_id", &self.rendezvous_id)
      .field("key", &"[redacted]")
      .field("device_name", &self.device_name)
      .field("platform", &self.platform)
      .finish()
  }
}

impl std::fmt::Debug for DeviceLinkRequest {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("DeviceLinkRequest")
      .field("id", &self.id)
      .field("key", &"[redacted]")
      .field("device_name", &self.device_name)
      .field("platform", &self.platform)
      .field("expires_in_secs", &self.expires_in_secs)
      .finish()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn wire_format_is_stable() {
    assert_eq!(
      serde_json::to_string(&DeviceLinkClosedReason::Claimed).unwrap(),
      r#""claimed""#
    );
    assert_eq!(
      serde_json::from_str::<DeviceLinkClosedReason>(r#""later""#).unwrap(),
      DeviceLinkClosedReason::Unknown
    );
    assert_eq!(
      serde_json::from_str::<DeviceLinkPlatform>(r#""haiku""#).unwrap(),
      DeviceLinkPlatform::Other
    );
    assert_eq!(
      serde_json::to_string(&DeviceLinkPlatform::Ios).unwrap(),
      r#""ios""#
    );
  }

  #[test]
  fn the_key_is_not_printed() {
    let request = DeviceLinkRequest {
      id: "ab".into(),
      key: Base64Bytes::new(vec![7; 32]),
      device_name: "Phone".into(),
      platform: DeviceLinkPlatform::Android,
      expires_in_secs: 300,
    };
    assert!(!format!("{request:?}").contains("BwcH"));
    assert!(format!("{request:?}").contains("redacted"));
  }
}
