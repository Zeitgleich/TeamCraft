//! The browser's half of the message sounds: Web Audio plays them, `decodeAudioData` checks and
//! decodes the own file, and IndexedDB keeps it (teamcraft-core's `own_sound_*`). Every sound is
//! made or decoded once per page and kept; an `AudioContext` exists only while a sound plays, so
//! nothing holds the audio device between messages.
//!
//! Browsers let a page start sound only after the user interacted with it (notifications.md,
//! footnote 1): a tab nobody touched since it loaded stays quiet, and its notifications keep the
//! system's sound ([`may_play`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use futures_util::future::{Either, select};
use js_sys::{ArrayBuffer, Reflect};
use teamcraft_client_api::message_sound::{
  BuiltInSound, MessageSound, OwnSoundInfo, OwnSoundLimits, OwnSoundPick, OwnSoundRefusal, OwnSoundStatus,
  SoundPlayed,
};
use teamcraft_core::platform::sleep;
use teamcraft_media::audio_frame::SAMPLE_RATE_HZ;
use teamcraft_media::message_sounds;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AudioBuffer, AudioContext, AudioContextState, Blob, File, OfflineAudioContext};

use crate::web_calls::js_text;

/// How long a context may take to start before the browser counts as blocking sound.
const RESUME_TIMEOUT: Duration = Duration::from_secs(1);
/// The context stays open a little after the samples: the device's own buffer.
const PLAYBACK_TAIL: Duration = Duration::from_millis(150);
/// Samples at or below this count as silence.
const SILENCE: f32 = 1e-4;

thread_local! {
  static CACHE: RefCell<PageCache> = RefCell::new(PageCache::default());
}

#[derive(Default)]
struct PageCache {
  built_in: HashMap<BuiltInSound, Rc<Vec<f32>>>,
  /// The own file as last decoded or found broken, for one set of limits.
  maybe_own: Option<OwnLoad>,
}

#[derive(Clone)]
struct OwnLoad {
  limits: OwnSoundLimits,
  result: Result<OwnSound, OwnSoundStatus>,
}

#[derive(Clone)]
struct OwnSound {
  samples: Rc<Vec<f32>>,
  info: OwnSoundInfo,
}

/// Why a picked file can't be kept.
enum DecodeFailure {
  Unreadable(String),
  NotAudio(String),
}

/// Whether this tab may start sound: browsers allow it once the user interacted with the page
/// (`navigator.userActivation.hasBeenActive`). A browser without that API is assumed to allow it;
/// a context it keeps suspended then just stays quiet.
pub fn may_play() -> bool {
  let Some(window) = web_sys::window() else {
    return false;
  };
  let activation =
    Reflect::get(&window.navigator(), &JsValue::from_str("userActivation")).unwrap_or(JsValue::UNDEFINED);
  if activation.is_undefined() || activation.is_null() {
    return true;
  }
  Reflect::get(&activation, &JsValue::from_str("hasBeenActive"))
    .ok()
    .and_then(|value| value.as_bool())
    .unwrap_or(true)
}

/// An `AudioContext`, made and resumed now: inside a click, that is what lets a browser play
/// (Safari strictly).
pub fn start_context() -> Option<AudioContext> {
  let context = AudioContext::new()
    .inspect_err(|error| tracing::debug!(error = %js_text(error), "no AudioContext"))
    .ok()?;
  // The promise settles on its own; `play` waits for the state.
  let _ = context.resume();
  Some(context)
}

/// Plays `sound` on `maybe_context` (or a new one) and closes the context when it has played.
pub async fn play(
  maybe_context: Option<AudioContext>,
  sound: MessageSound,
  limits: OwnSoundLimits,
) -> SoundPlayed {
  let Some(context) = maybe_context.or_else(start_context) else {
    return SoundPlayed::Quiet;
  };
  let (samples, played) = samples_for(sound, limits).await;
  if samples.is_empty() || !is_running(&context).await {
    close(&context);
    return SoundPlayed::Quiet;
  }
  if let Err(error) = start(&context, &samples) {
    tracing::debug!(error = %js_text(&error), "the message sound did not start");
    close(&context);
    return SoundPlayed::Quiet;
  }
  let length = Duration::from_secs_f32(samples.len() as f32 / SAMPLE_RATE_HZ as f32);
  sleep(length + PLAYBACK_TAIL).await;
  close(&context);
  played
}

/// Checks a picked file (size, then decoding) and keeps it in IndexedDB; a refused file leaves the
/// kept one as it was. `Err` when the browser would not store it.
pub async fn keep(file: File, limits: OwnSoundLimits) -> Result<OwnSoundPick, String> {
  if file.size() > limits.max_bytes() as f64 {
    return Ok(refused(OwnSoundRefusal::TooLarge));
  }
  let samples = match decode(&file, limits.max_play()).await {
    Ok(samples) => samples,
    Err(DecodeFailure::Unreadable(detail)) => {
      tracing::info!(%detail, "a picked sound file could not be read");
      return Ok(refused(OwnSoundRefusal::Unreadable));
    },
    Err(DecodeFailure::NotAudio(detail)) => {
      tracing::info!(%detail, "a picked sound file was refused");
      return Ok(refused(OwnSoundRefusal::NotAudio));
    },
  };
  teamcraft_core::store::own_sound_put(&file)
    .await
    .map_err(|error| format!("could not keep the sound file: {error}"))?;
  let own = OwnSound {
    info: OwnSoundInfo {
      file_name: file.name(),
      play_ms: message_sounds::play_ms(&samples),
    },
    samples: Rc::new(samples),
  };
  let info = own.info.clone();
  // async-ok: a thread-local, not a signal.
  CACHE.with(|cache| {
    cache.borrow_mut().maybe_own = Some(OwnLoad {
      limits,
      result: Ok(own),
    })
  });
  Ok(OwnSoundPick::Stored(info))
}

/// Looks at the kept file again (it may have gone: site data cleared).
pub async fn status(limits: OwnSoundLimits) -> OwnSoundStatus {
  forget_own();
  match own(limits).await.result {
    Ok(own) => OwnSoundStatus::Ready(own.info),
    Err(status) => status,
  }
}

pub async fn remove() -> Result<(), String> {
  forget_own();
  teamcraft_core::store::own_sound_delete()
    .await
    .map_err(|error| error.to_string())
}

/// The samples to play for `sound`, and whether the own file had to fall back.
async fn samples_for(sound: MessageSound, limits: OwnSoundLimits) -> (Rc<Vec<f32>>, SoundPlayed) {
  match sound {
    // Until a file is chosen the default sound plays; a file that can't play is a fallback the
    // settings mention.
    MessageSound::OwnFile => match own(limits).await.result {
      Ok(own) => (own.samples, SoundPlayed::Played),
      Err(OwnSoundStatus::NotPicked) => (built_in(BuiltInSound::DEFAULT), SoundPlayed::Played),
      Err(_) => (built_in(BuiltInSound::DEFAULT), SoundPlayed::FellBack),
    },
    MessageSound::BuiltIn(sound) => (built_in(sound), SoundPlayed::Played),
    MessageSound::System | MessageSound::Silent => (Rc::new(Vec::new()), SoundPlayed::Quiet),
  }
}

fn built_in(sound: BuiltInSound) -> Rc<Vec<f32>> {
  if let Some(samples) = CACHE.with(|cache| cache.borrow().built_in.get(&sound).cloned()) {
    return samples;
  }
  let samples = Rc::new(message_sounds::synthesize(sound));
  CACHE.with(|cache| cache.borrow_mut().built_in.insert(sound, samples.clone()));
  samples
}

/// The own file for `limits`: decoded once, then from memory (also when it can't play, so a broken
/// file isn't read again for every message).
async fn own(limits: OwnSoundLimits) -> OwnLoad {
  let maybe_cached = CACHE.with(|cache| {
    cache
      .borrow()
      .maybe_own
      .clone()
      .filter(|load| load.limits == limits)
  });
  if let Some(load) = maybe_cached {
    return load;
  }
  let load = OwnLoad {
    limits,
    result: load_own(limits).await,
  };
  // async-ok: a thread-local, not a signal.
  CACHE.with(|cache| cache.borrow_mut().maybe_own = Some(load.clone()));
  load
}

async fn load_own(limits: OwnSoundLimits) -> Result<OwnSound, OwnSoundStatus> {
  let file = match teamcraft_core::store::own_sound_get().await {
    Ok(Some(file)) => file,
    Ok(None) => return Err(OwnSoundStatus::NotPicked),
    Err(error) => {
      tracing::info!(%error, "the own message sound can't be read");
      return Err(OwnSoundStatus::Unavailable);
    },
  };
  let file_name = file.dyn_ref::<File>().map(File::name).unwrap_or_default();
  match decode(&file, limits.max_play()).await {
    Ok(samples) => Ok(OwnSound {
      info: OwnSoundInfo {
        file_name,
        play_ms: message_sounds::play_ms(&samples),
      },
      samples: Rc::new(samples),
    }),
    Err(DecodeFailure::Unreadable(detail) | DecodeFailure::NotAudio(detail)) => {
      tracing::info!(%detail, "the own message sound can't be played");
      Err(OwnSoundStatus::Unavailable)
    },
  }
}

/// Decodes a sound file into mono 48 kHz samples, at most `max` long (cut with a fade). The browser
/// decodes (and resamples) the whole file; only the part that plays is copied out of it.
async fn decode(file: &Blob, max: Duration) -> Result<Vec<f32>, DecodeFailure> {
  let bytes: ArrayBuffer = JsFuture::from(file.array_buffer())
    .await
    .map_err(|error| DecodeFailure::Unreadable(js_text(&error)))?
    .unchecked_into();
  let not_audio = |error: JsValue| DecodeFailure::NotAudio(js_text(&error));
  // Only for decoding: an offline context renders nothing and never touches the audio device.
  let context =
    OfflineAudioContext::new_with_number_of_channels_and_length_and_sample_rate(1, 1, SAMPLE_RATE_HZ as f32)
      .map_err(not_audio)?;
  let decoded: AudioBuffer = JsFuture::from(context.decode_audio_data(&bytes).map_err(not_audio)?)
    .await
    .map_err(not_audio)?
    .dyn_into()
    .map_err(not_audio)?;
  let max_samples = (max.as_secs_f32() * SAMPLE_RATE_HZ as f32).ceil() as usize;
  let length = (decoded.length() as usize).min(max_samples + 1);
  let channels = decoded.number_of_channels().max(1);
  let mut mono = vec![0.0f32; length];
  let mut channel_samples = vec![0.0f32; length];
  for channel in 0..channels {
    let channel = i32::try_from(channel).map_err(|_| DecodeFailure::NotAudio("channels".to_owned()))?;
    decoded
      .copy_from_channel(&mut channel_samples, channel)
      .map_err(not_audio)?;
    for (mixed, sample) in mono.iter_mut().zip(&channel_samples) {
      *mixed += sample / channels as f32;
    }
  }
  if mono.iter().all(|sample| sample.abs() <= SILENCE) {
    return Err(DecodeFailure::NotAudio("no sound in it".to_owned()));
  }
  message_sounds::cut_to(&mut mono, max);
  Ok(mono)
}

/// Waits (bounded) until the context runs: a browser that blocks sound keeps it suspended.
async fn is_running(context: &AudioContext) -> bool {
  if context.state() == AudioContextState::Running {
    return true;
  }
  if let Ok(promise) = context.resume() {
    // Either way the state below decides; a rejected resume leaves it suspended.
    let resumed = Box::pin(JsFuture::from(promise));
    if let Either::Right(_) = select(resumed, Box::pin(sleep(RESUME_TIMEOUT))).await {
      tracing::debug!("the browser keeps sound off in this tab");
    }
  }
  context.state() == AudioContextState::Running
}

fn start(context: &AudioContext, samples: &[f32]) -> Result<(), JsValue> {
  let length = u32::try_from(samples.len()).map_err(|_| JsValue::from_str("too long"))?;
  let buffer = context.create_buffer(1, length, SAMPLE_RATE_HZ as f32)?;
  buffer.copy_to_channel(samples, 0)?;
  let source = context.create_buffer_source()?;
  source.set_buffer(Some(&buffer));
  source.connect_with_audio_node(&context.destination())?;
  source.start()
}

/// Closing releases the audio device; a context that is closed already throws.
fn close(context: &AudioContext) {
  let _ = context.close();
}

fn forget_own() {
  CACHE.with(|cache| cache.borrow_mut().maybe_own = None);
}

fn refused(reason: OwnSoundRefusal) -> OwnSoundPick {
  OwnSoundPick::Refused { reason }
}
