//! The ringtone of an incoming call in the browser (notifications.md, "Calls"): the desktop's
//! synthesized tone (`teamcraft_media::ringtone`), looped with Web Audio while the call rings.
//!
//! Browsers let a page play sound only after the user interacted with it; a tab nobody touched
//! since it loaded stays quiet and shows the incoming call card and notification only.

use std::cell::RefCell;

use teamcraft_media::audio_frame::SAMPLE_RATE_HZ;
use teamcraft_media::ringtone;
use web_sys::{AudioBufferSourceNode, AudioContext};

thread_local! {
  static PLAYING: RefCell<Option<Ringing>> = const { RefCell::new(None) };
}

struct Ringing {
  context: AudioContext,
  _source: AudioBufferSourceNode,
}

/// Starts or stops the ringtone.
pub fn set_ringing(is_ringing: bool) {
  if !is_ringing {
    if let Some(ringing) = PLAYING.with(|playing| playing.borrow_mut().take()) {
      // Closing releases the audio device; a context that is closed already throws.
      let _ = ringing.context.close();
    }
    return;
  }
  if PLAYING.with(|playing| playing.borrow().is_some()) {
    return;
  }
  match start() {
    Ok(ringing) => PLAYING.with(|playing| *playing.borrow_mut() = Some(ringing)),
    Err(error) => tracing::debug!(?error, "the ringtone can't play in this tab"),
  }
}

fn start() -> Result<Ringing, wasm_bindgen::JsValue> {
  let context = AudioContext::new()?;
  let samples = ringtone::synthesize();
  let buffer = context.create_buffer(1, samples.len() as u32, SAMPLE_RATE_HZ as f32)?;
  buffer.copy_to_channel(&samples, 0)?;
  let source = context.create_buffer_source()?;
  source.set_buffer(Some(&buffer));
  source.set_loop(true);
  source.connect_with_audio_node(&context.destination())?;
  source.start()?;
  Ok(Ringing {
    context,
    _source: source,
  })
}
