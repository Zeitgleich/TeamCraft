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
//! with the page's callbacks and the `worklet` URL:
//!
//! | Callback             | Arguments                                  | Answer                                |
//! |----------------------|--------------------------------------------|---------------------------------------|
//! | `changed`            | `CallSnapshot` as JSON                     | –                                     |
//! | `prepare`            | conversation id (JSON)                     | promise of a [`PreparedCall`] (JSON)  |
//! | `sign`               | call id (JSON), DAVE public key (bytes)    | signed binding (bytes), or throws     |
//! | `verify`             | call id, device id (JSON), binding (bytes) | the bound DAVE key (bytes), or throws |
//! | `background_rule`    | –                                          | promise of `OrgBackgrounds` (JSON)    |
//! | `background_picture` | a `BackgroundEffect` (JSON)                | promise of the picture file (bytes)   |
//!
//! A rejoin after a lost connection prepares again (fresh token, current devices).
//!
//! Background effects (#439): before the camera starts, and when the user picks another effect,
//! the module asks `background_rule` for the organization's rule ([`crate::calls::OrgBackgrounds`])
//! and `background_picture` for the picture the effect the rule leaves needs (the user's own,
//! decrypted, or the organization's, hash-checked: the core has both). A page without them gets no
//! effects. The segmentation runs in a worker of its own ([`SEGMENTATION_WORKER_META`]), which the
//! module starts when the first effect starts; the worker downloads the model, a file of the build
//! ([`SEGMENTATION_MODEL_META`]), and checks it against the SHA-256 it was built with. How the
//! module composites depends on the browser ([`browser_effects_compositing`]).
//!
//! Sharing the screen takes two steps (issue #930): Safari opens the screen picker only while the
//! click's user activation lasts, which the action queue and the module's tasks outlast. So the
//! page calls [`EXPORT_PICK_SCREEN`] synchronously in the click handler, which opens the picker at
//! once, and hands the promise it answers to [`EXPORT_SHARE_SCREEN`] when the action's turn comes.
//!
//! Failed actions reject with [`crate::failure::ActionFailure`] wire text. An action that went
//! through resolves with nothing, or with the wire text of a part of it that failed without
//! undoing it ([`crate::remote::CallActionAnswer`]).

use serde::{Deserialize, Serialize};
use teamcraft_proto::{DeviceId, UserId};

pub use crate::call_view::CallSignal;

/// `<meta name>` under which the page finds the module's URL: the build names it after its content
/// (`scripts/finish-calls.sh`), so a page never loads another build's module.
pub const MODULE_META: &str = "teamcraft-calls";
/// `<meta name>` with the URL of the call's AudioWorklet module, served from the page's origin (the
/// web client's Content-Security-Policy allows no blob: scripts). The page hands it to the module
/// as the `worklet` property of the callbacks.
pub const WORKLET_META: &str = "teamcraft-call-worklet";
/// `<meta name>` with the URL of the person segmentation model for background effects
/// (`crates/teamcraft-media/models`), named after its content by the build
/// (`scripts/finish-calls.sh`). A build without the model has none: no effects then.
pub const SEGMENTATION_MODEL_META: &str = "teamcraft-segmentation-model";
/// `<meta name>` with the URL of the segmentation worker's script (`teamcraft-web-segmentation`,
/// a module worker of the page's origin), named after its content by the build
/// (`scripts/build-segmentation.sh`). A build without it has no effects.
pub const SEGMENTATION_WORKER_META: &str = "teamcraft-segmentation-worker";
/// `start(callbacks)`: once, after the module is initialized.
pub const EXPORT_START: &str = "start";
/// `act(CallAction JSON) -> Promise<undefined | string>` (see the module docs).
pub const EXPORT_ACT: &str = "act";
/// `signal(CallSignal JSON)`: a call event from the gateway.
pub const EXPORT_SIGNAL: &str = "signal";
/// `ring(bool)`: starts or stops the ringtone.
pub const EXPORT_RING: &str = "ring";
/// `watch_video(MediaStreamId JSON) -> canvas`: a new canvas that shows another device's track.
pub const EXPORT_WATCH_VIDEO: &str = "watch_video";
/// `unwatch_video(MediaStreamId JSON)`.
pub const EXPORT_UNWATCH_VIDEO: &str = "unwatch_video";
/// `camera_view() -> video | undefined`: a new `<video>` playing the camera this device sends, or
/// the pre-join check's camera.
pub const EXPORT_CAMERA_VIEW: &str = "camera_view";
/// `preview_level() -> number`: the microphone's level in the pre-join check (#442), from 0
/// (silence, or no check running) to 1.
pub const EXPORT_PREVIEW_LEVEL: &str = "preview_level";
/// `video_stats() -> Promise<string | undefined>`: what the call's video does now, as
/// [`crate::video_stats::VideoStatsReport`] JSON (stats for nerds, #938); nothing outside a call.
pub const EXPORT_VIDEO_STATS: &str = "video_stats";
/// `network_changed()`: the page's core saw the network come back or change, or the page woke up
/// (`online`, `visibilitychange`, `pageshow`, `resume`); a running call checks its session.
pub const EXPORT_NETWORK_CHANGED: &str = "network_changed";
/// `stop() -> Promise`: leaves a running call; the page is signing out.
pub const EXPORT_STOP: &str = "stop";
/// `pick_screen(ScreenContentMode JSON, has_sound, ScreenShareResolution JSON) -> Promise<MediaStream>`:
/// opens the browser's screen picker at once (`getDisplayMedia`), offering the tab's or the
/// system's sound with `has_sound` (#441; left out, no sound), for a share of the resolution
/// (#565; left out, standard); call it synchronously in the click handler. Throws when
/// the browser can't open one; the page then falls back to [`EXPORT_ACT`], which reports why.
pub const EXPORT_PICK_SCREEN: &str = "pick_screen";
/// `share_screen(has_sound, ScreenContentMode JSON, Promise<MediaStream>, ScreenShareResolution JSON)
/// -> Promise<undefined>` (the resolution left out: standard):
/// shares the screen chosen in a picker [`EXPORT_PICK_SCREEN`] opened, as `act` would carry out
/// `StartScreenShare` (a closed picker is no failure). The module owns the stream from here: it
/// stops it when the call ended meanwhile or the share fails.
pub const EXPORT_SHARE_SCREEN: &str = "share_screen";

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
  /// The workspace requires an organization background behind the camera image (admin.md §3.5):
  /// the camera starts only with one applied. The module asks `background` for the rule itself;
  /// this is what a page without that callback tells it (the camera stays off then).
  #[serde(default)]
  pub requires_background: bool,
  /// The SFU certificates' SHA-256s (hex) when they are self-signed (development, tests).
  #[serde(default)]
  pub certificate_sha256: Vec<String>,
}

/// How the browser's call module composites the background effects (#439).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectsCompositing {
  /// WebGPU, read back as I420 for the encoders: Chrome, Edge, Safari 26.
  WebGpu,
  /// WebGL2, whose canvas the encoders take: Firefox and browsers without WebGPU.
  WebGl2,
}

/// What a browser offers that the background effects need, as the page finds it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BrowserGraphics {
  pub has_webgpu: bool,
  pub has_webgl2: bool,
  /// WebAssembly SIMD: the segmentation module is built with it.
  pub has_wasm_simd: bool,
}

/// How this browser composites the background effects (#439), or `None` where it can't run them.
/// WebGPU where there is one, except in Firefox: it reads a frame back from the GPU only about
/// every 80 ms (measured with the effects' compositor, 720p, Firefox 147), far too slow for video,
/// so it composites with WebGL2 like browsers without WebGPU. The segmentation needs WebAssembly
/// SIMD (every browser with WebTransport has it). `user_agent` is `navigator.userAgent`.
pub fn browser_effects_compositing(
  graphics: BrowserGraphics,
  user_agent: &str,
) -> Option<EffectsCompositing> {
  if !graphics.has_wasm_simd {
    return None;
  }
  let is_firefox = user_agent.contains("Firefox/");
  if graphics.has_webgpu && !is_firefox {
    Some(EffectsCompositing::WebGpu)
  } else if graphics.has_webgl2 {
    Some(EffectsCompositing::WebGl2)
  } else {
    None
  }
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
  fn effects_composite_with_webgpu_where_it_reads_back_in_time_else_with_webgl2() {
    let chrome = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36";
    let safari = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.6.2 Safari/605.1.15";
    let firefox = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:147.0) Gecko/20100101 Firefox/147.0";
    let all = BrowserGraphics {
      has_webgpu: true,
      has_webgl2: true,
      has_wasm_simd: true,
    };
    let compositing = |graphics, user_agent| browser_effects_compositing(graphics, user_agent);
    assert_eq!(compositing(all, chrome), Some(EffectsCompositing::WebGpu));
    assert_eq!(compositing(all, safari), Some(EffectsCompositing::WebGpu));
    assert_eq!(compositing(all, firefox), Some(EffectsCompositing::WebGl2));
    let no_webgpu = BrowserGraphics {
      has_webgpu: false,
      ..all
    };
    assert_eq!(
      compositing(no_webgpu, chrome),
      Some(EffectsCompositing::WebGl2),
      "Chrome without WebGPU (Linux)"
    );
    let no_gpu = BrowserGraphics {
      has_wasm_simd: true,
      ..BrowserGraphics::default()
    };
    assert_eq!(compositing(no_gpu, firefox), None, "neither WebGPU nor WebGL2");
    assert_eq!(
      compositing(
        BrowserGraphics {
          has_webgl2: true,
          ..no_gpu
        },
        firefox
      ),
      Some(EffectsCompositing::WebGl2)
    );
    let no_simd = BrowserGraphics {
      has_wasm_simd: false,
      ..all
    };
    assert_eq!(compositing(no_simd, chrome), None, "the segmentation needs SIMD");
  }

  #[test]
  fn a_prepared_call_round_trips_and_hides_its_token() {
    let prepared = PreparedCall {
      sfu_url: "https://sfu.example.com:4433/call".into(),
      token: "secret".into(),
      user_id: id_from_raw(1),
      device_id: id_from_raw(10),
      trusted_devices: vec![id_from_raw(20)],
      requires_background: false,
      certificate_sha256: Vec::new(),
    };
    let json = serde_json::to_string(&prepared).unwrap();
    assert_eq!(serde_json::from_str::<PreparedCall>(&json).unwrap(), prepared);
    assert!(!format!("{prepared:?}").contains("secret"));
  }
}
