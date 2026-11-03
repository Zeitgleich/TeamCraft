//! The pre-join screen's speaker test in the browser: the test sound (`teamcraft_media::ringtone`)
//! once, through Web Audio, on the speakers the user picked.
//!
//! Browsers let a page start sound only from a user gesture (Safari strictly: the `AudioContext`
//! has to be created or resumed inside the click handler, not after an `await`). The button calls
//! [`prime`] synchronously in its handler, which creates the context and resumes it; [`play`]
//! takes that context later. It runs in the page, outside the call module and its action queue,
//! so a join right after the test doesn't wait for it.
//!
//! The context is routed with `AudioContext.setSinkId`; a browser without it, or a device it
//! refuses, plays on the default output and the test fails with
//! [`CallFailure::SpeakersUnavailable`], so the user learns why the sound came from elsewhere. A
//! context the browser keeps suspended fails with [`CallFailure::SoundBlocked`] instead of playing
//! nothing. The context is closed when the sound has played, so nothing holds the audio device.

use std::cell::RefCell;
use std::time::Duration;

use futures_util::future::select;
use js_sys::{Function, Promise, Reflect};
use teamcraft_client_api::failure::{ActionFailure, CallFailure, FailureKind};
use teamcraft_media::audio_frame::SAMPLE_RATE_HZ;
use teamcraft_media::ringtone;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AudioContext, AudioContextState};

use crate::web_calls::js_text;

/// How long a suspended context may take to start before the browser counts as blocking sound.
const RESUME_TIMEOUT: Duration = Duration::from_secs(1);
/// The context stays open a little after the samples: the device's own buffer.
const PLAYBACK_TAIL: Duration = Duration::from_millis(150);
/// The id browsers give the system's default output; `setSinkId` takes the empty string for it.
const DEFAULT_SINK_ID: &str = "default";

thread_local! {
  /// The context the last click created, until [`play`] takes it.
  static PRIMED: RefCell<Option<AudioContext>> = const { RefCell::new(None) };
}

/// Creates the test's `AudioContext` and starts it. Call it synchronously in the click handler,
/// before anything is awaited: that is what the browser counts as the user's gesture.
pub fn prime() {
  let Ok(context) = AudioContext::new() else {
    // `play` makes one later and reports it if sound is blocked.
    return;
  };
  // Resumed now, inside the gesture; the promise settles on its own.
  let _ = context.resume();
  if let Some(previous) = PRIMED.with(|primed| primed.borrow_mut().replace(context)) {
    // Closing releases the audio device; a context that is closed already throws.
    let _ = previous.close();
  }
}

/// Plays the test sound on `maybe_device` (a `MediaDeviceInfo.deviceId`; `None`, empty or
/// `"default"`: the default output) and returns once it has played.
pub async fn play(maybe_device: Option<String>) -> Result<(), ActionFailure> {
  let context = match PRIMED.with(|primed| primed.borrow_mut().take()) {
    Some(context) => context,
    None => AudioContext::new().map_err(|error| other("AudioContext", &error))?,
  };
  let result = play_in(&context, maybe_device.as_deref()).await;
  let _ = context.close();
  result
}

async fn play_in(context: &AudioContext, maybe_device: Option<&str>) -> Result<(), ActionFailure> {
  // Asked for before the first `await`, in case the context wasn't primed in the gesture.
  let maybe_resume = (context.state() != AudioContextState::Running)
    .then(|| context.resume().ok())
    .flatten();
  let maybe_sink_failure = match maybe_device.filter(|id| !id.is_empty() && *id != DEFAULT_SINK_ID) {
    Some(id) => set_sink(context, id).await.err(),
    None => None,
  };
  wait_until_running(context, maybe_resume).await?;
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

/// Waits (bounded) for a resume asked for earlier: a browser that blocks sound settles it late or
/// never.
async fn wait_until_running(
  context: &AudioContext,
  maybe_resume: Option<Promise>,
) -> Result<(), ActionFailure> {
  if context.state() != AudioContextState::Running
    && let Some(promise) = maybe_resume
  {
    // Either way the state below decides; a rejected resume leaves it suspended.
    let _ = select(JsFuture::from(promise), Box::pin(sleep(RESUME_TIMEOUT))).await;
  } else if context.state() != AudioContextState::Running {
    // Primed in the gesture: its resume is under way.
    sleep_until_running(context).await;
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

/// Checks a few times within [`RESUME_TIMEOUT`] whether the context started.
async fn sleep_until_running(context: &AudioContext) {
  const CHECKS: u32 = 10;
  for _ in 0..CHECKS {
    if context.state() == AudioContextState::Running {
      return;
    }
    sleep(RESUME_TIMEOUT / CHECKS).await;
  }
}

/// Waits on the page's event loop.
async fn sleep(duration: Duration) {
  let millis = i32::try_from(duration.as_millis()).unwrap_or(i32::MAX);
  let promise = Promise::new(&mut |resolve, _reject| {
    let is_set = web_sys::window().is_some_and(|window| {
      window
        .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis)
        .is_ok()
    });
    if !is_set {
      // No timer: don't wait forever.
      let _ = resolve.call0(&JsValue::NULL);
    }
  });
  // The promise only resolves.
  let _ = JsFuture::from(promise).await;
}

fn other(context: &str, error: &JsValue) -> ActionFailure {
  ActionFailure::other(format!("{context}: {}", js_text(error)))
}
