//! The browser call driver as a JS module for the SFU's development page (`/engine`).
//!
//! ```sh
//! cargo build -p teamcraft-media --example web_call --features web --release \
//!   --target wasm32-unknown-unknown
//! wasm-bindgen --target web --out-dir <dir> \
//!   target/wasm32-unknown-unknown/release/examples/web_call.wasm
//! TEAMCRAFT_SFU_DEV_ENGINE_DIR=<dir> cargo run -p teamcraft-sfu -- --dev
//! ```
//!
//! Development only: no device keys, so DAVE runs without call key bindings
//! (`UnauthenticatedKeys`). The app embeds [`WebCall`] with an authenticator instead.

use std::cell::RefCell;

use futures::channel::{mpsc, oneshot};
use futures::{StreamExt, future};
use js_sys::{Function, Object, Reflect};
use teamcraft_media::audio_io::{AudioDeviceChoice, AudioSink, AudioSource};
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_cues::StatusCues;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent, CallStats};
use teamcraft_media::call_resilience::ResilienceConfig;
use teamcraft_media::call_web::{WebCall, WebCallConfig};
use teamcraft_media::jitter::JitterBufferConfig;
use teamcraft_proto::{AudioCodec, DeviceId, MediaCapabilities, VideoCodec};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlCanvasElement, MediaStream};

/// A call driven from JS; events go to the `on_event` callback as strings.
#[wasm_bindgen]
pub struct DevCall {
  controls: mpsc::UnboundedSender<Control>,
  maybe_finished: RefCell<Option<oneshot::Receiver<Result<CallStats, String>>>>,
}

enum Control {
  Mute(bool),
  Deafen(bool),
  StartScreenShare {
    maybe_stream: Option<MediaStream>,
    done: oneshot::Sender<Result<(), String>>,
  },
  StopScreenShare,
  StartCamera(oneshot::Sender<Result<(), String>>),
  StopCamera,
  Leave,
}

/// Joins with `source` "tone:<hz>", "mic" or "silence"; `certificate_hash` may be empty. Other
/// devices' screen shares are drawn into `video_canvas`.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)] // A JS entry point; an options object would only move the list.
pub async fn join_dev_call(
  sfu_url: String,
  certificate_hash: Vec<u8>,
  token: String,
  user_id: String,
  device_id: String,
  source: String,
  on_event: Function,
  video_canvas: Option<HtmlCanvasElement>,
) -> Result<DevCall, JsError> {
  let device_id: DeviceId = device_id.parse().map_err(|_| JsError::new("invalid device id"))?;
  let engine = CallEngineConfig {
    user_id: user_id.parse().map_err(|_| JsError::new("invalid user id"))?,
    device_id,
    capabilities: MediaCapabilities {
      audio_codecs: vec![AudioCodec::Opus],
      video_encoders: vec![VideoCodec::H264],
      video_decoders: vec![VideoCodec::H264, VideoCodec::Vp9],
      max_simulcast_layers: 1,
    },
    bwe: BweConfig::default(),
  };
  let maybe_certificate_hash = match certificate_hash.len() {
    0 => None,
    _ => Some(
      <[u8; 32]>::try_from(certificate_hash.as_slice())
        .map_err(|_| JsError::new("certificate hash must be 32 bytes"))?,
    ),
  };
  let config = WebCallConfig {
    sfu_url,
    maybe_certificate_hash,
    token,
    engine,
    source: parse_source(&source)?,
    sink: AudioSink::Speakers(AudioDeviceChoice::Default),
    maybe_trusted_roster: None,
    maybe_authenticator: None,
    jitter: JitterBufferConfig::default(),
    resilience: ResilienceConfig::default(),
    // The test page has one dev token; it is reused for a rejoin while still valid.
    maybe_ticket_source: None,
    status_cues: StatusCues::default(),
    // The development page has no Content-Security-Policy.
    maybe_worklet_url: None,
  };
  let call = WebCall::join(config)
    .await
    .map_err(|error| JsError::new(&error.to_string()))?;
  let (controls, control_rx) = mpsc::unbounded();
  let (finished_tx, finished) = oneshot::channel();
  spawn_local(async move {
    let result = pump(call, control_rx, on_event, device_id, video_canvas).await;
    // The page may have been closed meanwhile.
    let _ = finished_tx.send(result);
  });
  Ok(DevCall {
    controls,
    maybe_finished: RefCell::new(Some(finished)),
  })
}

#[wasm_bindgen]
impl DevCall {
  pub fn set_muted(&self, is_muted: bool) {
    let _ = self.controls.unbounded_send(Control::Mute(is_muted));
  }

  pub fn set_deafened(&self, is_deafened: bool) {
    let _ = self.controls.unbounded_send(Control::Deafen(is_deafened));
  }

  /// Shares `stream` (e.g. a canvas' `captureStream()`), or asks for a screen or window when it
  /// is `undefined`.
  pub async fn start_screen_share(&self, maybe_stream: Option<MediaStream>) -> Result<(), JsError> {
    let (done, done_rx) = oneshot::channel();
    let _ = self
      .controls
      .unbounded_send(Control::StartScreenShare { maybe_stream, done });
    done_rx
      .await
      .map_err(|_| JsError::new("the call ended"))?
      .map_err(|error| JsError::new(&error))
  }

  pub fn stop_screen_share(&self) {
    let _ = self.controls.unbounded_send(Control::StopScreenShare);
  }

  /// Turns the camera on (three simulcast layers).
  pub async fn start_camera(&self) -> Result<(), JsError> {
    let (done, done_rx) = oneshot::channel();
    let _ = self.controls.unbounded_send(Control::StartCamera(done));
    done_rx
      .await
      .map_err(|_| JsError::new("the call ended"))?
      .map_err(|error| JsError::new(&error))
  }

  pub fn stop_camera(&self) {
    let _ = self.controls.unbounded_send(Control::StopCamera);
  }

  /// Leaves and resolves to `{ frames_sent, frames_received, audible_frames_played, privacy_code }`.
  pub async fn leave(&self) -> Result<Object, JsError> {
    let _ = self.controls.unbounded_send(Control::Leave);
    let finished = self
      .maybe_finished
      .borrow_mut()
      .take()
      .ok_or_else(|| JsError::new("already left"))?;
    let stats = finished
      .await
      .map_err(|_| JsError::new("the call ended"))?
      .map_err(|error| JsError::new(&error))?;
    let object = Object::new();
    let set = |key: &str, value: JsValue| Reflect::set(&object, &JsValue::from_str(key), &value);
    set("frames_sent", JsValue::from(stats.frames_sent as f64))
      .and_then(|_| set("frames_received", JsValue::from(stats.frames_received as f64)))
      .and_then(|_| {
        set(
          "audible_frames_played",
          JsValue::from(stats.audible_frames_played as f64),
        )
      })
      .and_then(|_| set("privacy_code", JsValue::from(stats.maybe_privacy_code)))
      .map_err(|_| JsError::new("building the statistics failed"))?;
    Ok(object)
  }
}

/// Owns the call: forwards events to JS and controls to the call until it is left.
async fn pump(
  mut call: WebCall,
  mut controls: mpsc::UnboundedReceiver<Control>,
  on_event: Function,
  own_device: DeviceId,
  maybe_video_canvas: Option<HtmlCanvasElement>,
) -> Result<CallStats, String> {
  loop {
    // Resolve to owned values first so the pending half (borrowing `call`) is dropped.
    let next = match future::select(controls.next(), Box::pin(call.next_event())).await {
      future::Either::Left((control, _)) => future::Either::Left(control),
      future::Either::Right((event, _)) => future::Either::Right(event),
    };
    match next {
      future::Either::Left(Some(Control::Mute(is_muted))) => call.set_muted(is_muted),
      future::Either::Left(Some(Control::Deafen(is_deafened))) => call.set_deafened(is_deafened),
      future::Either::Left(Some(Control::StartScreenShare { maybe_stream, done })) => {
        let result = match maybe_stream {
          Some(stream) => call.share_video_stream(stream).await,
          None => call.start_screen_share().await,
        }
        .map_err(|error| error.to_string());
        let _ = done.send(result);
      },
      future::Either::Left(Some(Control::StopScreenShare)) => call.stop_screen_share(),
      future::Either::Left(Some(Control::StartCamera(done))) => {
        let result = call.start_camera(None).await.map_err(|error| error.to_string());
        let _ = done.send(result);
      },
      future::Either::Left(Some(Control::StopCamera)) => call.stop_camera(),
      future::Either::Left(Some(Control::Leave) | None) | future::Either::Right(None) => break,
      // Every 100 ms; too chatty for the event log.
      future::Either::Right(Some(CallEngineEvent::UplinkEstimate { .. })) => {},
      future::Either::Right(Some(event)) => {
        if let (CallEngineEvent::VideoPublished(track), Some(canvas)) = (&event, &maybe_video_canvas)
          && track.device_id != own_device
        {
          call.watch_video(track.stream_id, canvas.clone());
        }
        // A throwing callback is the page's problem; the call goes on.
        let _ = on_event.call1(&JsValue::NULL, &JsValue::from_str(&describe(&event)));
      },
    }
  }
  call.leave().await.map_err(|error| error.to_string())
}

fn describe(event: &CallEngineEvent) -> String {
  match event {
    CallEngineEvent::Secured { epoch, privacy_code } => {
      format!("secured: epoch {epoch}, privacy code {privacy_code}")
    },
    other => format!("{other:?}"),
  }
}

fn parse_source(value: &str) -> Result<AudioSource, JsError> {
  Ok(match value.split_once(':') {
    Some(("tone", frequency)) => AudioSource::Tone {
      frequency_hz: frequency
        .parse()
        .map_err(|_| JsError::new("invalid tone frequency"))?,
      amplitude: 0.3,
    },
    None if value == "mic" => AudioSource::Microphone(AudioDeviceChoice::Default),
    None if value == "silence" => AudioSource::Silence,
    _ => return Err(JsError::new("source must be tone:<hz>, mic or silence")),
  })
}
