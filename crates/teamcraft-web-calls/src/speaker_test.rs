//! The pre-join screen's speaker test in the browser: the test sound (`teamcraft_media::ringtone`)
//! once, through Web Audio, on the speakers the user picked.
//!
//! Each test has its own `AudioContext`, closed when the sound has played, so nothing holds the
//! audio device afterwards. The context is routed with `AudioContext.setSinkId`; a browser without
//! it, or a device it refuses, plays on the default output and the test fails with
//! [`CallFailure::SpeakersUnavailable`], so the user learns why the sound came from elsewhere. A
//! context the browser keeps suspended (no interaction with the page yet) fails with
//! [`CallFailure::SoundBlocked`] instead of playing nothing.

use std::time::Duration;

use futures::future::select;
use js_sys::{Function, Promise, Reflect};
use teamcraft_client_api::failure::{ActionFailure, CallFailure, FailureKind};
use teamcraft_media::audio_frame::SAMPLE_RATE_HZ;
use teamcraft_media::ringtone;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AudioContext, AudioContextState};

use crate::js_text;

/// How long a suspended context may take to start before the browser counts as blocking sound.
const RESUME_TIMEOUT: Duration = Duration::from_secs(1);
/// The context stays open a little after the samples: the device's own buffer.
const PLAYBACK_TAIL: Duration = Duration::from_millis(150);
/// The id browsers give the system's default output; `setSinkId` takes the empty string for it.
const DEFAULT_SINK_ID: &str = "default";

/// Plays the test sound on `maybe_device` (a `MediaDeviceInfo.deviceId`; `None`, empty or
/// `"default"`: the default output) and returns once it has played.
pub async fn play(maybe_device: Option<&str>) -> Result<(), ActionFailure> {
  let context = AudioContext::new().map_err(|error| other("AudioContext", &error))?;
  let result = play_in(&context, maybe_device).await;
  // Closing releases the audio device; a context that is closed already throws.
  let _ = context.close();
  result
}

async fn play_in(context: &AudioContext, maybe_device: Option<&str>) -> Result<(), ActionFailure> {
  let maybe_sink_failure = match maybe_device.filter(|id| !id.is_empty() && *id != DEFAULT_SINK_ID) {
    Some(id) => set_sink(context, id).await.err(),
    None => None,
  };
  start_running(context).await?;
  let samples = ringtone::test_sound();
  let length = Duration::from_secs_f32(samples.len() as f32 / SAMPLE_RATE_HZ as f32);
  let buffer = context
    .create_buffer(1, samples.len() as u32, SAMPLE_RATE_HZ as f32)
    .map_err(|error| other("AudioBuffer", &error))?;
  buffer
    .copy_to_channel(&samples, 0)
    .map_err(|error| other("AudioBuffer", &error))?;
  let source = context
    .create_buffer_source()
    .map_err(|error| other("AudioBufferSourceNode", &error))?;
  source.set_buffer(Some(&buffer));
  source
    .connect_with_audio_node(&context.destination())
    .map_err(|error| other("audio graph", &error))?;
  source
    .start()
    .map_err(|error| other("AudioBufferSourceNode", &error))?;
  sleep(length + PLAYBACK_TAIL).await;
  match maybe_sink_failure {
    None => Ok(()),
    Some(detail) => Err(ActionFailure::new(
      FailureKind::Call(CallFailure::SpeakersUnavailable),
      detail,
    )),
  }
}

/// Routes the context to the speakers; the error says why it stays on the default output.
async fn set_sink(context: &AudioContext, id: &str) -> Result<(), String> {
  let set_sink_id = Reflect::get(context, &JsValue::from_str("setSinkId"))
    .ok()
    .and_then(|value| value.dyn_into::<Function>().ok())
    .ok_or_else(|| "this browser can't choose the speakers (no AudioContext.setSinkId)".to_owned())?;
  let promise: Promise = set_sink_id
    .call1(context, &JsValue::from_str(id))
    .and_then(JsCast::dyn_into)
    .map_err(|error| format!("setSinkId: {}", js_text(&error)))?;
  JsFuture::from(promise)
    .await
    .map(|_| ())
    .map_err(|error| format!("setSinkId: {}", js_text(&error)))
}

/// Starts a suspended context. Browsers start one only after the user interacted with the page;
/// otherwise the promise settles late or never, so the wait is bounded.
async fn start_running(context: &AudioContext) -> Result<(), ActionFailure> {
  if context.state() != AudioContextState::Running
    && let Ok(promise) = context.resume()
  {
    let resumed = JsFuture::from(promise);
    // Either way the state below decides; a rejected resume leaves it suspended.
    let _ = select(resumed, Box::pin(sleep(RESUME_TIMEOUT))).await;
  }
  if context.state() == AudioContextState::Running {
    Ok(())
  } else {
    Err(ActionFailure::new(
      FailureKind::Call(CallFailure::SoundBlocked),
      "the browser keeps the AudioContext suspended",
    ))
  }
}

/// Waits on the page's event loop.
async fn sleep(duration: Duration) {
  let millis = i32::try_from(duration.as_millis()).unwrap_or(i32::MAX);
  let promise = Promise::new(&mut |resolve, _reject| match web_sys::window() {
    Some(window) => {
      if window
        .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis)
        .is_err()
      {
        // No timer: don't wait forever.
        let _ = resolve.call0(&JsValue::NULL);
      }
    },
    None => {
      let _ = resolve.call0(&JsValue::NULL);
    },
  });
  // The promise only resolves.
  let _ = JsFuture::from(promise).await;
}

fn other(context: &str, error: &JsValue) -> ActionFailure {
  ActionFailure::other(format!("{context}: {}", js_text(error)))
}
