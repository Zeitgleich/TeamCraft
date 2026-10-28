//! Background effects behind the browser's camera (#439). The organization's rule and the pictures
//! come from the page's core (the `background_rule` and `background_picture` callbacks). The
//! segmentation runs in a worker of its own (`teamcraft-web-segmentation`), started when the first
//! effect starts and kept for the session: only then are its module and the model downloaded, and
//! the model is checked and parsed once. The compositing is `teamcraft-media`'s: WebGPU, or WebGL2
//! where WebGPU reads frames back too slowly (Firefox) or isn't there.

use std::cell::RefCell;
use std::rc::Rc;

use teamcraft_client_api::calls::BackgroundEffect as ChosenEffect;
use teamcraft_client_api::page_calls::{
  EffectsCompositing, SEGMENTATION_MODEL_META, SEGMENTATION_WORKER_META,
};
use teamcraft_media::call_web::{CameraEffects, SegmentationWorker};
use teamcraft_media::effects::{
  BackgroundEffect, BackgroundImage, EffectsError, chosen_background, model_url, worker_url,
};
use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, MediaStream};

/// Frames a second the self view's canvas stream offers at most (the camera's rate).
const VIEW_FRAMERATE: f64 = 30.0;

/// The segmentation worker and the last chosen effect, for the session.
#[derive(Default)]
pub(crate) struct Effects {
  maybe_worker: RefCell<Option<Rc<SegmentationWorker>>>,
  /// The last chosen effect as the pipeline's: drawing an illustration or decoding a picture
  /// takes a moment, and own and organization pictures come from the server.
  maybe_last: RefCell<Option<(ChosenEffect, BackgroundEffect)>>,
}

impl Effects {
  /// The pipeline's effect for `chosen` if it was the last one asked for.
  pub fn cached(&self, chosen: ChosenEffect) -> Option<BackgroundEffect> {
    self
      .maybe_last
      .borrow()
      .as_ref()
      .filter(|(last, _)| *last == chosen)
      .map(|(_, effect)| effect.clone())
  }

  /// The pipeline's effect for `chosen`, drawn or from `maybe_picture` (decoded), kept for the
  /// next time.
  pub fn effect(
    &self,
    chosen: ChosenEffect,
    maybe_picture: Option<BackgroundImage>,
  ) -> Result<BackgroundEffect, EffectsError> {
    if let Some(effect) = self.cached(chosen) {
      return Ok(effect);
    }
    let effect = chosen_background(chosen, maybe_picture)?;
    *self.maybe_last.borrow_mut() = Some((chosen, effect.clone()));
    Ok(effect)
  }

  /// The camera's effects: the segmentation worker (started the first time) and a compositor.
  /// WebGPU that can't start (no adapter, a refused GPU) falls back to WebGL2.
  pub async fn camera_effects(
    &self,
    compositing: EffectsCompositing,
    effect: BackgroundEffect,
  ) -> Result<CameraEffects, EffectsError> {
    let worker = self.worker().await?;
    match compositing {
      EffectsCompositing::WebGpu => match CameraEffects::webgpu(Rc::clone(&worker), effect.clone()).await {
        Ok(effects) => Ok(effects),
        Err(error) => {
          tracing::warn!(%error, "WebGPU can't composite the effects; trying WebGL2");
          CameraEffects::webgl(worker, effect)
        },
      },
      EffectsCompositing::WebGl2 => CameraEffects::webgl(worker, effect),
    }
  }

  /// The running worker, or a new one (the first time, or after it failed).
  async fn worker(&self) -> Result<Rc<SegmentationWorker>, EffectsError> {
    let maybe_running = self
      .maybe_worker
      .borrow()
      .as_ref()
      .filter(|worker| worker.maybe_failure().is_none())
      .cloned();
    if let Some(worker) = maybe_running {
      return Ok(worker);
    }
    let script = worker_url(meta_content(SEGMENTATION_WORKER_META).as_deref())?;
    let model = model_url(meta_content(SEGMENTATION_MODEL_META).as_deref())?;
    let worker = Rc::new(SegmentationWorker::start(&script, &model).await?);
    *self.maybe_worker.borrow_mut() = Some(Rc::clone(&worker));
    Ok(worker)
  }
}

/// A canvas the effects draw every sent picture into, and the stream of it the self view plays.
pub(crate) fn self_view() -> Option<(HtmlCanvasElement, MediaStream)> {
  let canvas: HtmlCanvasElement = web_sys::window()?
    .document()?
    .create_element("canvas")
    .ok()?
    .dyn_into()
    .ok()?;
  let stream = canvas
    .capture_stream_with_frame_request_rate(VIEW_FRAMERATE)
    .ok()?;
  Some((canvas, stream))
}

/// The `content` of `<meta name="…">` in the page.
fn meta_content(name: &str) -> Option<String> {
  web_sys::window()?
    .document()?
    .query_selector(&format!("meta[name=\"{name}\"]"))
    .ok()??
    .get_attribute("content")
}
