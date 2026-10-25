//! The contract between the web client and its call module (`teamcraft-web-calls`).
//!
//! The browser's calls are a wasm module of their own: the call state the UI shows, the ringtone
//! and the media (WebTransport, WebCodecs, Web Audio, DAVE). The page loads it in the background
//! once the user is signed in, so it doesn't count against the app's first download
//! (robustness.md, "Download size"). The page keeps the client core and the device key; the module
//! asks it for what needs them. They talk through JavaScript: JSON text for these types, functions
//! for the rest.
//!
//! The module exports [`EXPORT_START`] and the other `EXPORT_*` functions. `start` takes an object
//! with the page's callbacks:
//!
//! | Callback  | Arguments                                  | Answer                                  |
//! |-----------|--------------------------------------------|-----------------------------------------|
//! | `changed` | `CallSnapshot` as JSON                     | –                                       |
//! | `prepare` | conversation id (JSON)                     | promise of a [`PreparedCall`] (JSON)    |
//! | `sign`    | call id (JSON), DAVE public key (bytes)    | signed binding (bytes), or throws       |
//! | `verify`  | call id, device id (JSON), binding (bytes) | the bound DAVE key (bytes), or throws   |
//!
//! A rejoin after a lost connection prepares again (fresh token, current devices).
//!
//! Failed actions reject with [`crate::failure::ActionFailure`] wire text.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{DeviceId, UserId};

pub use crate::call_view::CallSignal;

/// Where the page finds the module (trunk builds it next to the app).
pub const MODULE_URL: &str = "/teamcraft-web-calls.js";
/// The call's AudioWorklet module, served from the page's origin: the web client's
/// Content-Security-Policy allows no blob: scripts.
pub const WORKLET_URL: &str = "/teamcraft-call-worklet.js";
/// `start(callbacks)`: once, after the module is initialized.
pub const EXPORT_START: &str = "start";
/// `act(CallAction JSON) -> Promise`.
pub const EXPORT_ACT: &str = "act";
/// `signal(CallSignal JSON)`: a call event from the gateway.
pub const EXPORT_SIGNAL: &str = "signal";
/// `ring(bool)`: starts or stops the ringtone.
pub const EXPORT_RING: &str = "ring";
/// `watch_video(MediaStreamId JSON) -> canvas`: a new canvas that shows another device's track.
pub const EXPORT_WATCH_VIDEO: &str = "watch_video";
/// `unwatch_video(MediaStreamId JSON)`.
pub const EXPORT_UNWATCH_VIDEO: &str = "unwatch_video";
/// `camera_view() -> video | undefined`: a new `<video>` playing the camera this device sends.
pub const EXPORT_CAMERA_VIEW: &str = "camera_view";
/// `stop() -> Promise`: leaves a running call; the page is signing out.
pub const EXPORT_STOP: &str = "stop";

/// What the page's client core prepared for joining a call (e2ee.md §8). The page keeps the
/// signer and the verified certificates and answers `sign` and `verify` with them.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedCall {
  pub sfu_url: String,
  pub token: String,
  pub user_id: UserId,
  pub device_id: DeviceId,
  /// The other devices whose certificates verified: only they may be in the call's DAVE group.
  pub trusted_devices: Vec<DeviceId>,
  /// The workspace requires an organization background behind the camera image (admin.md §3.5),
  /// which the browser can't apply: the camera stays off.
  #[serde(default)]
  pub requires_background: bool,
  /// The SFU certificate's SHA-256 (hex) when it is self-signed (development, tests).
  #[serde(default)]
  pub maybe_certificate_sha256: Option<String>,
}

// The join token doesn't belong in logs.
impl std::fmt::Debug for PreparedCall {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PreparedCall")
      .field("sfu_url", &self.sfu_url)
      .field("trusted_devices", &self.trusted_devices.len())
      .finish_non_exhaustive()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  #[test]
  fn a_prepared_call_round_trips_and_hides_its_token() {
    let prepared = PreparedCall {
      sfu_url: "https://sfu.example.com:4433/call".into(),
      token: "secret".into(),
      user_id: id_from_raw(1),
      device_id: id_from_raw(10),
      trusted_devices: vec![id_from_raw(20)],
      requires_background: false,
      maybe_certificate_sha256: None,
    };
    let json = serde_json::to_string(&prepared).unwrap();
    assert_eq!(serde_json::from_str::<PreparedCall>(&json).unwrap(), prepared);
    assert!(!format!("{prepared:?}").contains("secret"));
  }
}
