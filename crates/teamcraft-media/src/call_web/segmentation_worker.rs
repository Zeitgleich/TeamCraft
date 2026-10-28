//! The page's side of the segmentation worker (#439; the messages are in
//! [`crate::effects::worker_protocol`]).
//!
//! The worker runs the person segmentation (`teamcraft-web-segmentation`: tract and the model) off
//! the page's main thread. It is started when the first background effect starts, so its module
//! and the model are only downloaded then, and kept for the session: later starts of the effects
//! reuse it and its parsed model. The camera's effects hand it a small picture whenever it is idle
//! and use the newest mask it answered with; frames in between reuse the last one.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use futures::channel::oneshot;
use futures::future::{Either, select};
use js_sys::{Array, Float32Array, Object, Reflect, Uint8Array};
use tracing::warn;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{ErrorEvent, MessageEvent, Worker, WorkerOptions, WorkerType};

use super::{js_message, sleep};
use crate::effects::worker_protocol::{
  DETAIL, FAILED, HEIGHT, KIND, LOAD, LOADED, MASK, MILLISECONDS, PIXELS, READY, SEGMENT, URL, VALUES, WIDTH,
};
use crate::effects::{EffectsError, SegmentationMask};

/// How long the worker may take to start and load the model: both are downloads (about 1.6 MB and
/// 0.3 MB compressed) on a first start. Beyond it the effects count as unavailable.
const START_TIMEOUT: Duration = Duration::from_secs(60);

/// A running segmentation worker with its model loaded.
pub struct SegmentationWorker {
  worker: Worker,
  shared: Rc<RefCell<Shared>>,
  _on_message: Closure<dyn FnMut(MessageEvent)>,
  _on_error: Closure<dyn FnMut(ErrorEvent)>,
}

/// A mask the worker answered with, and how long it took there.
pub struct WorkerMask {
  pub mask: SegmentationMask,
  pub cost: Duration,
}

#[derive(Default)]
struct Shared {
  /// Waits for the next answer while starting ([`READY`], then [`LOADED`]).
  maybe_waiter: Option<oneshot::Sender<Result<(), String>>>,
  /// A picture is with the worker.
  is_busy: bool,
  /// The newest mask nobody took yet.
  maybe_mask: Option<WorkerMask>,
  /// The worker failed for good (its module trapped, or a segmentation failed).
  maybe_failure: Option<String>,
}

impl SegmentationWorker {
  /// Starts the worker from `script_url` (its module script, of the page's origin) and has it load
  /// the model at `model_url`.
  pub async fn start(script_url: &str, model_url: &str) -> Result<Self, EffectsError> {
    let options = WorkerOptions::new();
    options.set_type(WorkerType::Module);
    options.set_name("segmentation");
    let worker = Worker::new_with_options(script_url, &options).map_err(failed("starting the worker"))?;
    let shared = Rc::new(RefCell::new(Shared::default()));
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
        let detail = format!("the segmentation worker failed: {}", event.message());
        shared.borrow_mut().fail(detail);
      })
    };
    worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    let this = Self {
      worker,
      shared,
      _on_message: on_message,
      _on_error: on_error,
    };
    this.answer("starting the segmentation worker").await?;
    let load = message(LOAD, &[(URL, JsValue::from_str(model_url))]).map_err(failed("the worker message"))?;
    this
      .worker
      .post_message(&load)
      .map_err(failed("loading the model"))?;
    this.answer("loading the segmentation model").await?;
    Ok(this)
  }

  /// Whether the worker still works; one that failed is replaced at the next start of the effects.
  pub fn maybe_failure(&self) -> Option<String> {
    self.shared.borrow().maybe_failure.clone()
  }

  /// Whether a picture is with the worker (the next one waits).
  pub fn is_busy(&self) -> bool {
    self.shared.borrow().is_busy
  }

  /// Hands the worker an RGBA picture (`width` x `height`) to segment, unless it is busy with one;
  /// whether it took it. The mask comes back later ([`SegmentationWorker::take_mask`]).
  pub fn segment(&self, width: u32, height: u32, rgba: &[u8]) -> Result<bool, EffectsError> {
    if let Some(failure) = self.maybe_failure() {
      return Err(EffectsError::Segmentation(failure));
    }
    if self.is_busy() {
      return Ok(false);
    }
    let pixels = Uint8Array::from(rgba).buffer();
    let request = message(
      SEGMENT,
      &[
        (WIDTH, JsValue::from(width)),
        (HEIGHT, JsValue::from(height)),
        (PIXELS, pixels.clone().into()),
      ],
    )
    .map_err(failed("the worker message"))?;
    self
      .worker
      .post_message_with_transfer(&request, &Array::of1(&pixels))
      .map_err(failed("handing the worker a picture"))?;
    self.shared.borrow_mut().is_busy = true;
    Ok(true)
  }

  /// The newest mask since the last call, if one came.
  pub fn take_mask(&self) -> Option<WorkerMask> {
    self.shared.borrow_mut().maybe_mask.take()
  }

  /// Forgets a mask not taken yet (a camera starts again: it would show where the person was).
  pub fn forget_mask(&self) {
    self.shared.borrow_mut().maybe_mask = None;
  }

  /// Waits for the worker's answer to a start step.
  async fn answer(&self, what: &str) -> Result<(), EffectsError> {
    let (sender, receiver) = oneshot::channel();
    {
      let mut shared = self.shared.borrow_mut();
      if let Some(failure) = &shared.maybe_failure {
        return Err(EffectsError::Segmentation(failure.clone()));
      }
      shared.maybe_waiter = Some(sender);
    }
    match select(receiver, std::pin::pin!(sleep(START_TIMEOUT))).await {
      Either::Left((Ok(Ok(())), _)) => Ok(()),
      Either::Left((Ok(Err(detail)), _)) => Err(EffectsError::Segmentation(format!("{what}: {detail}"))),
      Either::Left((Err(_), _)) => Err(EffectsError::Segmentation(format!("{what}: no answer"))),
      Either::Right(_) => {
        self.shared.borrow_mut().maybe_waiter = None;
        Err(EffectsError::Segmentation(format!(
          "{what}: no answer within {} s",
          START_TIMEOUT.as_secs()
        )))
      },
    }
  }
}

impl Drop for SegmentationWorker {
  fn drop(&mut self) {
    self.worker.set_onmessage(None);
    self.worker.set_onerror(None);
    self.worker.terminate();
  }
}

impl Shared {
  fn on_message(&mut self, data: &JsValue) {
    let kind = field(data, KIND).as_string().unwrap_or_default();
    match kind.as_str() {
      READY | LOADED => self.answered(Ok(())),
      MASK => {
        self.is_busy = false;
        match mask_of(data) {
          Ok(mask) => self.maybe_mask = Some(mask),
          Err(detail) => self.fail(detail),
        }
      },
      FAILED => {
        let detail = field(data, DETAIL)
          .as_string()
          .unwrap_or_else(|| "unknown error".to_owned());
        if self.maybe_waiter.is_some() {
          self.answered(Err(detail));
        } else {
          // A segmentation that fails once fails again: the model is the same every time.
          self.fail(detail);
        }
      },
      other => warn!(kind = other, "unknown message from the segmentation worker"),
    }
  }

  fn answered(&mut self, result: Result<(), String>) {
    if let Some(waiter) = self.maybe_waiter.take() {
      // The starter may have given up waiting.
      let _ = waiter.send(result);
    }
  }

  fn fail(&mut self, detail: String) {
    self.is_busy = false;
    self.answered(Err(detail.clone()));
    if self.maybe_failure.is_none() {
      warn!(%detail, "the segmentation worker failed");
      self.maybe_failure = Some(detail);
    }
  }
}

fn mask_of(data: &JsValue) -> Result<WorkerMask, String> {
  let dimension = |name: &str| field(data, name).as_f64().map(|value| value as u32).unwrap_or(0);
  let values = field(data, VALUES)
    .dyn_into::<js_sys::ArrayBuffer>()
    .map_err(|_| "a mask without values".to_owned())?;
  let values = Float32Array::new(&values).to_vec();
  let mask =
    SegmentationMask::new(dimension(WIDTH), dimension(HEIGHT), values).map_err(|error| error.to_string())?;
  let millis = field(data, MILLISECONDS).as_f64().unwrap_or(0.0).max(0.0);
  Ok(WorkerMask {
    mask,
    cost: Duration::from_secs_f64(millis / 1000.0),
  })
}

fn field(data: &JsValue, name: &str) -> JsValue {
  Reflect::get(data, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
}

/// `{ kind, ...fields }`.
fn message(kind: &str, fields: &[(&str, JsValue)]) -> Result<Object, JsValue> {
  let object = Object::new();
  Reflect::set(&object, &JsValue::from_str(KIND), &JsValue::from_str(kind))?;
  for (name, value) in fields {
    Reflect::set(&object, &JsValue::from_str(name), value)?;
  }
  Ok(object)
}

fn failed(what: &'static str) -> impl Fn(JsValue) -> EffectsError {
  move |error| EffectsError::Segmentation(format!("{what}: {}", js_message(&error)))
}
