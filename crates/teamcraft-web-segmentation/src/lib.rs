//! The browser's person segmentation for background effects (#439): tract and the model in a wasm
//! module of its own, which runs in a dedicated worker.
//!
//! The call module starts the worker (`teamcraft_media::call_web::SegmentationWorker`) when the
//! first effect starts, so neither this module nor the model is downloaded before someone turns
//! an effect on, and the segmentation leaves the page's main thread. The messages are in
//! `teamcraft_media::effects::worker_protocol`: the worker says it is ready, loads the model the
//! page names (a file of the build, checked against the SHA-256 this module was built with), then
//! answers each picture with its person mask.
//!
//! `worker.js` is the worker's script: it loads this module, whose start function takes over the
//! worker's messages. The build (`crates/teamcraft-ui/scripts/build-segmentation.sh`) compiles the
//! module with WebAssembly SIMD, which every browser with WebTransport runs; the page doesn't offer
//! effects in one without it.
#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, ArrayBuffer, Float32Array, Object, Reflect, Uint8Array};
use teamcraft_media::effects::worker_protocol::{
  DETAIL, FAILED, HEIGHT, KIND, LOAD, LOADED, MASK, MILLISECONDS, PIXELS, READY, SEGMENT, URL, VALUES, WIDTH,
};
use teamcraft_media::effects::{EffectsError, ModelCache, TractSegmenter, model_url};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent, Response};

/// The loaded model, for the worker's life.
type Segmenter = Rc<RefCell<Option<TractSegmenter>>>;

/// Takes over the worker's messages and says it is ready.
#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
  console_error_panic_hook::set_once();
  let scope: DedicatedWorkerGlobalScope = js_sys::global().dyn_into()?;
  let segmenter: Segmenter = Rc::new(RefCell::new(None));
  let on_message = {
    let scope = scope.clone();
    Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
      on_message(&scope, &segmenter, &event.data());
    })
  };
  scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
  // The worker's handler for as long as it runs.
  on_message.forget();
  post(&scope, READY, &[], None);
  Ok(())
}

fn on_message(scope: &DedicatedWorkerGlobalScope, segmenter: &Segmenter, data: &JsValue) {
  match field(data, KIND).as_string().as_deref() {
    Some(LOAD) => {
      let url = field(data, URL).as_string().unwrap_or_default();
      let (scope, segmenter) = (scope.clone(), Rc::clone(segmenter));
      spawn_local(async move {
        match load(&scope, &url).await {
          Ok(loaded) => {
            *segmenter.borrow_mut() = Some(loaded);
            post(&scope, LOADED, &[], None);
          },
          Err(error) => failed(&scope, &error.to_string()),
        }
      });
    },
    Some(SEGMENT) => {
      let started = now(scope);
      let result = match segmenter.borrow_mut().as_mut() {
        Some(segmenter) => segment(segmenter, data),
        None => Err(EffectsError::Segmentation("no model loaded".into())),
      };
      match result {
        Ok((width, height, values)) => {
          let millis = now(scope) - started;
          let buffer = values.buffer();
          post(
            scope,
            MASK,
            &[
              (WIDTH, JsValue::from(width)),
              (HEIGHT, JsValue::from(height)),
              (VALUES, buffer.clone().into()),
              (MILLISECONDS, JsValue::from(millis)),
            ],
            Some(&buffer),
          );
        },
        Err(error) => failed(scope, &error.to_string()),
      }
    },
    other => failed(scope, &format!("unknown message {other:?}")),
  }
}

/// Downloads the model at `url` (a path of the page's origin), checks and parses it.
async fn load(scope: &DedicatedWorkerGlobalScope, url: &str) -> Result<TractSegmenter, EffectsError> {
  let url = model_url(Some(url))?;
  let failed = |what: &'static str| {
    let url = url.clone();
    move |error: JsValue| EffectsError::Segmentation(format!("{what} {url}: {error:?}"))
  };
  let response: Response = JsFuture::from(scope.fetch_with_str(&url))
    .await
    .map_err(failed("fetching"))?
    .unchecked_into();
  if !response.ok() {
    return Err(EffectsError::Segmentation(format!(
      "fetching {url}: HTTP {}",
      response.status()
    )));
  }
  let buffer = JsFuture::from(response.array_buffer().map_err(failed("reading"))?)
    .await
    .map_err(failed("reading"))?;
  ModelCache::pinned().store(&Uint8Array::new(&buffer).to_vec())
}

/// The mask of the picture in `data`: its size and the values.
fn segment(segmenter: &mut TractSegmenter, data: &JsValue) -> Result<(u32, u32, Float32Array), EffectsError> {
  let dimension = |name: &str| field(data, name).as_f64().map_or(0, |value| value as u32);
  let pixels = field(data, PIXELS)
    .dyn_into::<ArrayBuffer>()
    .map_err(|_| EffectsError::Segmentation("a picture without pixels".into()))?;
  let pixels = Uint8Array::new(&pixels).to_vec();
  let mask = segmenter.segment_rgba(dimension(WIDTH), dimension(HEIGHT), &pixels)?;
  Ok((mask.width(), mask.height(), Float32Array::from(mask.values())))
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
    web_sys::console::error_2(&JsValue::from_str("the segmentation worker can't answer"), &error);
  }
}

fn field(data: &JsValue, name: &str) -> JsValue {
  Reflect::get(data, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
}

fn now(scope: &DedicatedWorkerGlobalScope) -> f64 {
  scope.performance().map_or(0.0, |performance| performance.now())
}
