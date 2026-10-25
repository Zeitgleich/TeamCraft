//! The contract between the web client and its call module (`teamcraft-web-calls`).
//!
//! The browser's call media (WebTransport, WebCodecs, Web Audio, DAVE) is a wasm module of its own
//! that the page loads only when a call needs it, so it doesn't count against the app's download
//! (robustness.md, "Download size"). The page keeps the device key and the call roster; the module
//! keeps the media. They talk through JavaScript: JSON text for these types, functions for signing.
//!
//! The module exports [`EXPORT_START`] and the other `EXPORT_*` functions. `start` takes an object
//! with the page's callbacks:
//!
//! | Callback  | Arguments                                  | Answer                               |
//! |-----------|--------------------------------------------|--------------------------------------|
//! | `changed` | [`MediaCallState`] as JSON                 | –                                    |
//! | `sign`    | call id (JSON), DAVE public key (bytes)    | signed binding (bytes), or throws    |
//! | `verify`  | call id, device id (JSON), binding (bytes) | the bound DAVE key (bytes), or throws |
//! | `ticket`  | –                                          | promise of a `CallJoinTokenResponse` |
//!
//! Failed actions reject with [`crate::failure::ActionFailure`] wire text.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{DeviceId, UserId};

use crate::calls::{CallRequest, CallStatusSounds};

pub use crate::call_view::MediaCallState;

/// Where the page finds the module (trunk builds it next to the app).
pub const MODULE_URL: &str = "/teamcraft-web-calls.js";
/// The call's AudioWorklet module, served from the page's origin: the web client's
/// Content-Security-Policy allows no blob: scripts.
pub const WORKLET_URL: &str = "/teamcraft-call-worklet.js";
/// `start(callbacks)`: once, after the module is initialized.
pub const EXPORT_START: &str = "start";
/// `join(WebCallJoin JSON) -> Promise`.
pub const EXPORT_JOIN: &str = "join";
/// `act(CallAction JSON) -> Promise`: every action but joining.
pub const EXPORT_ACT: &str = "act";
/// `devices() -> Promise<MediaDevices JSON>`.
pub const EXPORT_DEVICES: &str = "devices";
/// `watch_video(MediaStreamId JSON, canvas)`: draws another device's track into the canvas.
pub const EXPORT_WATCH_VIDEO: &str = "watch_video";
/// `unwatch_video(MediaStreamId JSON)`.
pub const EXPORT_UNWATCH_VIDEO: &str = "unwatch_video";
/// `local_camera() -> MediaStream | undefined`: the camera this device sends, for the self view.
pub const EXPORT_LOCAL_CAMERA: &str = "local_camera";
/// `stop()`: leaves a running call; the page is signing out.
pub const EXPORT_STOP: &str = "stop";

/// Everything the module needs to join a call; the page prepared it with the client core.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebCallJoin {
  pub request: CallRequest,
  pub sfu_url: String,
  pub token: String,
  pub user_id: UserId,
  pub device_id: DeviceId,
  /// The other devices whose certificates verified: only they may be in the call's DAVE group.
  pub trusted_devices: Vec<DeviceId>,
  /// `MediaDeviceInfo.deviceId`s; `None` follows the browser's default.
  #[serde(default)]
  pub maybe_microphone: Option<String>,
  #[serde(default)]
  pub maybe_speakers: Option<String>,
  #[serde(default)]
  pub status_sounds: CallStatusSounds,
}

// The join token doesn't belong in logs.
impl std::fmt::Debug for WebCallJoin {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("WebCallJoin")
      .field("request", &self.request)
      .field("sfu_url", &self.sfu_url)
      .field("trusted_devices", &self.trusted_devices.len())
      .finish_non_exhaustive()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::calls::CallKind;
  use crate::model::id_from_raw;

  #[test]
  fn a_join_without_device_choices_reads_with_defaults() {
    let join = WebCallJoin {
      request: CallRequest {
        conversation: id_from_raw(5),
        kind: CallKind::Direct,
      },
      sfu_url: "https://sfu.example.com:4433/call".into(),
      token: "secret".into(),
      user_id: id_from_raw(1),
      device_id: id_from_raw(10),
      trusted_devices: vec![id_from_raw(20)],
      maybe_microphone: None,
      maybe_speakers: None,
      status_sounds: CallStatusSounds::default(),
    };
    let mut value = serde_json::to_value(&join).unwrap();
    let object = value.as_object_mut().unwrap();
    for optional in ["maybe_microphone", "maybe_speakers", "status_sounds"] {
      object.remove(optional);
    }
    assert_eq!(serde_json::from_value::<WebCallJoin>(value).unwrap(), join);
    assert!(!format!("{join:?}").contains("secret"));
  }
}
