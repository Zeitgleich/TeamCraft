//! Background effects behind the browser's camera (#439). The organization's rule and the pictures
//! come from the page's core (the `background_rule` and `background_picture` callbacks); the
//! segmentation model is a file of the build, downloaded when the first effect starts, checked
//! against the SHA-256 this module was built with and kept for the session; the pipeline is
//! `teamcraft-media`'s (tract on the main thread, the WebGPU compositor).

use std::cell::RefCell;
use std::time::Duration;

use js_sys::Uint8Array;
use teamcraft_client_api::calls::BackgroundEffect as ChosenEffect;
use teamcraft_client_api::page_calls::SEGMENTATION_MODEL_META;
use teamcraft_media::effects::{
  BackgroundEffect, Compositor, EffectsConfig, EffectsError, EffectsPipeline, MaskCadence, ModelCache,
  TractSegmenter, chosen_background, model_url,
};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{HtmlCanvasElement, MediaStream, Response};

/// The segmentation's share of a frame's time on the page's main thread (a frame lasts 33 ms at
/// 30 fps): a slower segmenter runs every second or third frame instead (`MaskCadence`).
const SEGMENTATION_BUDGET: Duration = Duration::from_millis(8);
/// Frames a second the self view's canvas stream offers at most (the camera's rate).
const VIEW_FRAMERATE: f64 = 30.0;

/// The model and the last chosen effect, for the session.
#[derive(Default)]
pub(crate) struct Effects {
  maybe_model: RefCell<Option<ModelCache>>,
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

  /// The pipeline's effect for `chosen`, drawn or decoded from `maybe_picture`, kept for the next
  /// time.
  pub fn effect(
    &self,
    chosen: ChosenEffect,
    maybe_picture: Option<&[u8]>,
  ) -> Result<BackgroundEffect, EffectsError> {
    if let Some(effect) = self.cached(chosen) {
      return Ok(effect);
    }
    let effect = chosen_background(chosen, maybe_picture)?;
    *self.maybe_last.borrow_mut() = Some((chosen, effect.clone()));
    Ok(effect)
  }

  /// A pipeline for the camera: the model (downloaded the first time), the GPU compositor.
  pub async fn pipeline(&self, effect: BackgroundEffect) -> Result<EffectsPipeline, EffectsError> {
    let segmenter = self.segmenter().await?;
    let compositor = Compositor::new(EffectsConfig::default()).await?;
    Ok(
      EffectsPipeline::new(Box::new(segmenter), compositor, effect)
        .with_cadence(MaskCadence::within(SEGMENTATION_BUDGET)),
    )
  }

  async fn segmenter(&self) -> Result<TractSegmenter, EffectsError> {
    let maybe_cached = self.maybe_model.borrow().as_ref().and_then(ModelCache::cached);
    if let Some(segmenter) = maybe_cached {
      return Ok(segmenter);
    }
    let url = model_url(meta_content(SEGMENTATION_MODEL_META).as_deref())?;
    let file = download(&url).await?;
    let mut model = self.maybe_model.borrow_mut();
    model.get_or_insert_with(ModelCache::pinned).store(&file)
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

/// The file at `url` of the page's origin (the server sends it compressed and cached for good: its
/// name has its hash).
async fn download(url: &str) -> Result<Vec<u8>, EffectsError> {
  let failed = |what: &'static str| {
    move |error: wasm_bindgen::JsValue| EffectsError::Segmentation(format!("{what} {url}: {error:?}"))
  };
  let window = web_sys::window().ok_or_else(|| EffectsError::Segmentation("no window".into()))?;
  let response: Response = JsFuture::from(window.fetch_with_str(url))
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
  Ok(Uint8Array::new(&buffer).to_vec())
}

/// The `content` of `<meta name="…">` in the page.
fn meta_content(name: &str) -> Option<String> {
  web_sys::window()?
    .document()?
    .query_selector(&format!("meta[name=\"{name}\"]"))
    .ok()??
    .get_attribute("content")
}
