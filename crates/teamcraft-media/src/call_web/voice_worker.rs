//! The page's side of the voice worker (#2140; the messages are in [`crate::voice_fec::protocol`]).
//!
//! WebCodecs conceals a lost voice packet; the worker (`teamcraft-web-voice`: `ropus`) recovers it
//! from the next packet's in-band FEC data, as the native receivers do. [`VoiceFec`] starts the
//! worker when a voice packet first goes missing, so a call without loss never downloads its
//! module. Once it is ready, each remote voice moves over to it at its next decode (primed with the
//! packets WebCodecs decoded last, so the switch is not heard) and stays there. A worker that fails
//! hands its voices back to WebCodecs, and is started again on a later loss, no sooner than
//! [`RETRY_AFTER`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use futures::channel::mpsc;
use js_sys::{Array, ArrayBuffer, Float32Array, Object, Reflect, Uint8Array};
use tracing::{debug, info, warn};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{ErrorEvent, MessageEvent, Worker, WorkerOptions, WorkerType};
use web_time::Instant;

use super::{Input, RemoteStream, js_message};
use crate::voice_fec::protocol::{
  CLOSE, CONCEAL, DECODE, DECODED, DETAIL, FAILED, KIND, NEXT, PACKET, PCM, PRIME, READY, SAMPLES, STREAM,
};

/// How long after a failed worker the next loss may start one again.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// The call's voice worker: not started, starting or running, or failed a while ago.
pub(super) struct VoiceFec {
  /// The worker's script (the build's `<meta name="teamcraft-voice-worker">`); none, and voices
  /// stay with WebCodecs.
  maybe_url: Option<String>,
  maybe_worker: Option<VoiceWorker>,
  maybe_failed_at: Option<Instant>,
}

/// A running (or starting) voice worker; clones share it. Dropping the last one ends it.
#[derive(Clone)]
pub(super) struct VoiceWorker(Rc<WorkerInner>);

/// One remote voice on the worker; dropping it frees the worker's decoder.
pub(super) struct VoiceStream {
  worker: VoiceWorker,
  number: u32,
}

struct WorkerInner {
  worker: Worker,
  shared: Rc<RefCell<Shared>>,
  _on_message: Closure<dyn FnMut(MessageEvent)>,
  _on_error: Closure<dyn FnMut(ErrorEvent)>,
}

struct Shared {
  is_ready: bool,
  maybe_failure: Option<String>,
  /// The remote stream of each number on the worker, for its answers.
  streams: HashMap<u32, RemoteStream>,
  next_number: u32,
  inputs: mpsc::UnboundedSender<Input>,
}

impl VoiceFec {
  pub(super) fn new(maybe_url: Option<String>) -> Self {
    Self {
      maybe_url,
      maybe_worker: None,
      maybe_failed_at: None,
    }
  }

  /// A voice packet went missing: starts the worker, unless it runs or failed a short while ago.
  pub(super) fn on_voice_lost(&mut self, now: Instant, inputs: &mpsc::UnboundedSender<Input>) {
    if self.maybe_worker.is_some()
      || self
        .maybe_failed_at
        .is_some_and(|failed_at| now.saturating_duration_since(failed_at) < RETRY_AFTER)
    {
      return;
    }
    let Some(url) = self.maybe_url.as_deref() else {
      return;
    };
    match VoiceWorker::start(url, inputs.clone()) {
      Ok(worker) => {
        info!("a voice packet went missing: starting the voice decoder with FEC");
        self.maybe_worker = Some(worker);
      },
      Err(detail) => {
        warn!(%detail, "the voice decoder with FEC didn't start; lost voice packets stay concealed");
        self.maybe_failed_at = Some(now);
      },
    }
  }

  /// The worker, once it takes voices.
  pub(super) fn ready_worker(&self) -> Option<&VoiceWorker> {
    self.maybe_worker.as_ref().filter(|worker| worker.is_ready())
  }

  /// Whether the worker failed since the last call: it is dropped, and the voices on it go back to
  /// WebCodecs.
  pub(super) fn take_failure(&mut self, now: Instant) -> bool {
    let Some(failure) = self.maybe_worker.as_ref().and_then(VoiceWorker::maybe_failure) else {
      return false;
    };
    warn!(%failure, "the voice decoder with FEC failed; lost voice packets are concealed again");
    self.maybe_worker = None;
    self.maybe_failed_at = Some(now);
    true
  }
}

impl VoiceWorker {
  /// Starts the worker from `script_url` (its module script, of the page's origin); it takes voices
  /// once it said it is ready. Its answers go to `inputs` as [`Input::Decoded`].
  fn start(script_url: &str, inputs: mpsc::UnboundedSender<Input>) -> Result<Self, String> {
    let options = WorkerOptions::new();
    options.set_type(WorkerType::Module);
    options.set_name("voice");
    let worker = Worker::new_with_options(script_url, &options).map_err(|error| js_message(&error))?;
    let shared = Rc::new(RefCell::new(Shared {
      is_ready: false,
      maybe_failure: None,
      streams: HashMap::new(),
      next_number: 0,
      inputs,
    }));
    let on_message = {
      let shared = Rc::clone(&shared);
      Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        shared.borrow_mut().on_message(&event.data());
      })
    };
    let on_error = {
      let shared = Rc::clone(&shared);
      Closure::<dyn FnMut(ErrorEvent)>::new(move |event: ErrorEvent| {
        // A module that doesn't load (404, refused by the CSP) or traps.
        shared.borrow_mut().maybe_failure = Some(format!("the voice worker failed: {}", event.message()));
      })
    };
    worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    Ok(Self(Rc::new(WorkerInner {
      worker,
      shared,
      _on_message: on_message,
      _on_error: on_error,
    })))
  }

  fn is_ready(&self) -> bool {
    let shared = self.0.shared.borrow();
    shared.is_ready && shared.maybe_failure.is_none()
  }

  fn maybe_failure(&self) -> Option<String> {
    self.0.shared.borrow().maybe_failure.clone()
  }

  /// Moves `stream` onto the worker: its decoder first decodes `recent` (the packets the decoder
  /// it takes over from decoded last, oldest first) without answering.
  pub(super) fn open<'a>(
    &self,
    stream: RemoteStream,
    recent: impl IntoIterator<Item = &'a [u8]>,
  ) -> VoiceStream {
    let number = {
      let mut shared = self.0.shared.borrow_mut();
      let number = shared.next_number;
      shared.next_number = number.wrapping_add(1);
      shared.streams.insert(number, stream);
      number
    };
    let voice = VoiceStream {
      worker: self.clone(),
      number,
    };
    for packet in recent {
      voice.post(PRIME, &[(PACKET, Some(packet))], None);
    }
    debug!(sender = %stream.0, "the voice decoder with FEC takes over a voice");
    voice
  }

  fn post(&self, message: &Object, maybe_transfer: Option<&ArrayBuffer>) {
    let posted = match maybe_transfer {
      Some(buffer) => self
        .0
        .worker
        .post_message_with_transfer(message, &Array::of1(buffer)),
      None => self.0.worker.post_message(message),
    };
    if let Err(error) = posted {
      self.0.shared.borrow_mut().maybe_failure =
        Some(format!("posting to the voice worker: {}", js_message(&error)));
    }
  }
}

impl VoiceStream {
  /// Hands the worker a packet; its samples come back as [`Input::Decoded`].
  pub(super) fn decode(&self, packet: &[u8]) {
    self.post(DECODE, &[(PACKET, Some(packet))], None);
  }

  /// A packet of `lost_samples` went missing: recovered from `maybe_next`'s FEC data where it
  /// carries some, concealed otherwise. Comes back as [`Input::Decoded`] of that length.
  pub(super) fn conceal(&self, maybe_next: Option<&[u8]>, lost_samples: usize) {
    self.post(
      CONCEAL,
      &[(NEXT, maybe_next)],
      Some(JsValue::from(lost_samples as u32)),
    );
  }

  /// Posts `{ kind, stream, ...packets as ArrayBuffers, samples? }`, handing the buffers over.
  fn post(&self, kind: &str, packets: &[(&str, Option<&[u8]>)], maybe_samples: Option<JsValue>) {
    let message = Object::new();
    let transfer = Array::new();
    let set = |name: &str, value: &JsValue| {
      // Setting a field of a fresh plain object can't fail.
      let _ = Reflect::set(&message, &JsValue::from_str(name), value);
    };
    set(KIND, &JsValue::from_str(kind));
    set(STREAM, &JsValue::from(self.number));
    for (name, maybe_packet) in packets {
      if let Some(packet) = maybe_packet {
        let buffer = Uint8Array::from(*packet).buffer();
        transfer.push(&buffer);
        set(name, &buffer);
      }
    }
    if let Some(samples) = maybe_samples {
      set(SAMPLES, &samples);
    }
    let posted = self
      .worker
      .0
      .worker
      .post_message_with_transfer(&message, &transfer);
    if let Err(error) = posted {
      self.worker.0.shared.borrow_mut().maybe_failure =
        Some(format!("posting to the voice worker: {}", js_message(&error)));
    }
  }
}

impl Drop for VoiceStream {
  fn drop(&mut self) {
    self.worker.0.shared.borrow_mut().streams.remove(&self.number);
    let message = Object::new();
    let _ = Reflect::set(&message, &JsValue::from_str(KIND), &JsValue::from_str(CLOSE));
    let _ = Reflect::set(&message, &JsValue::from_str(STREAM), &JsValue::from(self.number));
    self.worker.post(&message, None);
  }
}

impl Drop for WorkerInner {
  fn drop(&mut self) {
    self.worker.set_onmessage(None);
    self.worker.set_onerror(None);
    self.worker.terminate();
  }
}

impl Shared {
  fn on_message(&mut self, data: &JsValue) {
    match field(data, KIND).as_string().as_deref() {
      Some(READY) => self.is_ready = true,
      Some(DECODED) => {
        let maybe_stream = field(data, STREAM)
          .as_f64()
          .and_then(|number| self.streams.get(&(number as u32)).copied());
        let pcm = field(data, PCM);
        let (Some(stream), Some(buffer)) = (maybe_stream, pcm.dyn_ref::<ArrayBuffer>()) else {
          // A stream closed meanwhile.
          return;
        };
        let samples = Float32Array::new(buffer).to_vec();
        // The call ended meanwhile: nobody plays the samples.
        let _ = self.inputs.unbounded_send(Input::Decoded { stream, samples });
      },
      Some(FAILED) => {
        let detail = field(data, DETAIL).as_string().unwrap_or_default();
        self.maybe_failure = Some(detail);
      },
      other => debug!(?other, "unknown message from the voice worker"),
    }
  }
}

fn field(data: &JsValue, name: &str) -> JsValue {
  Reflect::get(data, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
}
