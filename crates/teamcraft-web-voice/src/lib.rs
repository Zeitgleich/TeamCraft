//! The browser's voice decoder with Opus in-band FEC (#2140): `ropus` in a wasm module of its own,
//! which runs in a dedicated worker.
//!
//! WebCodecs can't decode an Opus packet's FEC data, so the call module decodes voice with
//! WebCodecs until a voice packet first goes missing, then starts this worker
//! (`teamcraft_media::call_web::VoiceWorker`) and hands its voices over to it: a call without loss
//! never downloads the module, and a guest's first download doesn't carry it. The messages are in
//! `teamcraft_media::voice_fec::protocol`: the worker says it is ready, then keeps one decoder per
//! stream number the page names and answers each packet with its samples.
//!
//! `worker.js` is the worker's script: it loads this module, whose start function takes over the
//! worker's messages. The build (`crates/teamcraft-ui/scripts/build-workers.sh`) compiles it with
//! WebAssembly SIMD, as the segmentation module.
#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::rc::Rc;

use js_sys::{Array, ArrayBuffer, Float32Array, Object, Reflect, Uint8Array};
use teamcraft_media::voice_fec::FecVoiceDecoder;
use teamcraft_media::voice_fec::protocol::{
  CLOSE, CONCEAL, DECODE, DECODED, DETAIL, FAILED, KIND, NEXT, PACKET, PCM, PRIME, READY, SAMPLES, STREAM,
};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

/// Decoders kept at most: a call's remote voices, with room to spare. The page closes each when
/// its stream ends; past this a stream number the page forgot can't grow the worker for ever.
const MAX_STREAMS: usize = 512;

/// One decoder per stream number.
type Decoders = Rc<RefCell<HashMap<u32, FecVoiceDecoder>>>;

/// Takes over the worker's messages and says it is ready.
#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
  console_error_panic_hook::set_once();
  let scope: DedicatedWorkerGlobalScope = js_sys::global().dyn_into()?;
  let decoders: Decoders = Rc::default();
  let on_message = {
    let scope = scope.clone();
    Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
      on_message(&scope, &decoders, &event.data());
    })
  };
  scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
  // The worker's handler for as long as it runs.
  on_message.forget();
  post(&scope, READY, &[], None);
  Ok(())
}

fn on_message(scope: &DedicatedWorkerGlobalScope, decoders: &Decoders, data: &JsValue) {
  let kind = field(data, KIND).as_string();
  let Some(stream) = field(data, STREAM).as_f64().map(|number| number as u32) else {
    failed(scope, &format!("a {kind:?} message without a stream"));
    return;
  };
  if kind.as_deref() == Some(CLOSE) {
    decoders.borrow_mut().remove(&stream);
    return;
  }
  let mut decoders = decoders.borrow_mut();
  if !decoders.contains_key(&stream) && decoders.len() >= MAX_STREAMS {
    failed(scope, &format!("more than {MAX_STREAMS} voices"));
    return;
  }
  let decoder = match decoders.entry(stream) {
    Entry::Occupied(entry) => entry.into_mut(),
    Entry::Vacant(entry) => match FecVoiceDecoder::new() {
      Ok(decoder) => entry.insert(decoder),
      Err(error) => {
        failed(scope, &error.to_string());
        return;
      },
    },
  };
  match kind.as_deref() {
    Some(PRIME) => {
      // Only the decoder's state counts: the page played this packet already.
      decoder.decode(&bytes(&field(data, PACKET)));
    },
    Some(DECODE) => {
      let pcm = decoder.decode(&bytes(&field(data, PACKET)));
      answer(scope, stream, &pcm);
    },
    Some(CONCEAL) => {
      let next = field(data, NEXT);
      let maybe_next = (!next.is_undefined() && !next.is_null()).then(|| bytes(&next));
      let lost = field(data, SAMPLES)
        .as_f64()
        .map_or(decoder.last_packet_samples(), |samples| samples as usize);
      let pcm = decoder.conceal(maybe_next.as_deref(), lost);
      answer(scope, stream, &pcm);
    },
    other => failed(scope, &format!("unknown message {other:?}")),
  }
}

/// Posts a packet's samples, handing over their buffer.
fn answer(scope: &DedicatedWorkerGlobalScope, stream: u32, pcm: &[f32]) {
  let buffer = Float32Array::from(pcm).buffer();
  post(
    scope,
    DECODED,
    &[(STREAM, JsValue::from(stream)), (PCM, buffer.clone().into())],
    Some(&buffer),
  );
}

/// The bytes of an `ArrayBuffer` field; nothing for anything else (decoded as a loss).
fn bytes(value: &JsValue) -> Vec<u8> {
  value
    .dyn_ref::<ArrayBuffer>()
    .map(|buffer| Uint8Array::new(buffer).to_vec())
    .unwrap_or_default()
}

fn failed(scope: &DedicatedWorkerGlobalScope, detail: &str) {
  post(scope, FAILED, &[(DETAIL, JsValue::from_str(detail))], None);
}

/// Posts `{ kind, ...fields }`, handing over `maybe_transfer`.
fn post(
  scope: &DedicatedWorkerGlobalScope,
  kind: &str,
  fields: &[(&str, JsValue)],
  maybe_transfer: Option<&ArrayBuffer>,
) {
  let message = Object::new();
  let _ = Reflect::set(&message, &JsValue::from_str(KIND), &JsValue::from_str(kind));
  for (name, value) in fields {
    let _ = Reflect::set(&message, &JsValue::from_str(name), value);
  }
  let posted = match maybe_transfer {
    Some(buffer) => scope.post_message_with_transfer(&message, &Array::of1(buffer)),
    None => scope.post_message(&message),
  };
  if let Err(error) = posted {
    web_sys::console::error_2(&JsValue::from_str("the voice worker can't answer"), &error);
  }
}

fn field(data: &JsValue, name: &str) -> JsValue {
  Reflect::get(data, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
}
