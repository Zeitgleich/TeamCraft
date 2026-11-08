//! The browser's background effects, timed (#1336): the call module's pipeline (`call_web`'s
//! `CameraEffects`, the segmentation worker as the release builds it) on a `<video>` the page
//! plays, one picture every 33 ms, each encoded with WebCodecs H.264 the way a call's encoders take
//! them (a picture is skipped while more than two wait in the encoder). Compares the WebGPU and the
//! WebGL2 compositing in the same browser. Nothing leaves the page.
//!
//! `dev/effects-bench/build.sh` builds it, the worker and the page; `dev/effects-bench/README.md`
//! says how to run it in a browser. The page calls `run(video, options)`, which resolves to the
//! summary as JSON (`call_web::TimedEffects::summary`) with the encoder's output added.
//! Options: `compositing` ("webgpu" or "webgl2"), `frames` (default 165), `worker` and `model`
//! (URLs), `effect` ("strong", "light" or "none").
#![cfg(target_arch = "wasm32")]

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use js_sys::Reflect;
use teamcraft_media::call_web::{CameraEffects, SegmentationWorker, TimedEffects};
use teamcraft_media::effects::{BackgroundEffect, BlurStrength};
use teamcraft_media::web_api::{VideoEncoder, dictionary};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::HtmlVideoElement;
use web_time::Instant;

/// One picture every this long (30 fps, as a camera).
const PERIOD: Duration = Duration::from_millis(33);
/// As the call's encoders: a picture is skipped while more than this many wait.
const MAX_ENCODE_QUEUE: u32 = 2;
/// A call's top camera layer at 720p.
const BITRATE_BPS: u32 = 2_500_000;

/// Runs the bench on `video` (playing) and resolves to its summary (JSON).
#[wasm_bindgen]
pub async fn run(video: HtmlVideoElement, options: JsValue) -> Result<JsValue, JsValue> {
  let text = |key: &str| {
    Reflect::get(&options, &key.into())
      .ok()
      .and_then(|value| value.as_string())
  };
  let compositing = text("compositing").unwrap_or_else(|| "webgpu".to_owned());
  let frames = Reflect::get(&options, &"frames".into())
    .ok()
    .and_then(|value| value.as_f64())
    .map_or(165, |frames| frames as u32);
  let effect = match text("effect").as_deref() {
    Some("none") => BackgroundEffect::None,
    Some("light") => BackgroundEffect::Blur(BlurStrength::Light),
    _ => BackgroundEffect::Blur(BlurStrength::Strong),
  };
  let worker_url = text("worker").ok_or("options.worker: the segmentation worker's script")?;
  let model_url = text("model").ok_or("options.model: the segmentation model")?;

  let started = Instant::now();
  let worker = Rc::new(
    SegmentationWorker::start(&worker_url, &model_url)
      .await
      .map_err(|error| error.to_string())?,
  );
  let worker_start_ms = started.elapsed().as_secs_f64() * 1000.0;
  let effects = match compositing.as_str() {
    "webgl2" => CameraEffects::webgl(worker, effect),
    _ => CameraEffects::webgpu(worker, effect).await,
  }
  .map_err(|error| error.to_string())?;
  let mut pipeline = TimedEffects::new(effects).map_err(|error| error.to_string())?;

  let (width, height) = (video.video_width() & !7, video.video_height() & !1);
  let chunks = Rc::new(Cell::new(0_u32));
  let first_chunk = Rc::new(Cell::new(None::<Instant>));
  let failure = Rc::new(Cell::new(None::<String>));
  let encoder = encoder(width, height, &chunks, &first_chunk, &failure)?;

  let mut given = 0_u64;
  let mut skipped = 0_u64;
  let mut failed_pictures = 0_u32;
  let mut maybe_first_encode = None::<Instant>;
  let run_started = Instant::now();
  for index in 0..frames {
    let picture_started = Instant::now();
    let timestamp_us = f64::from(index) * PERIOD.as_secs_f64() * 1_000_000.0;
    match pipeline.frame(&video, timestamp_us).await {
      Ok(frame) => {
        if encoder.encode_queue_size() > MAX_ENCODE_QUEUE {
          skipped += 1;
        } else {
          let keyframe = dictionary(&[("keyFrame", JsValue::from_bool(index == 0))])?;
          encoder.encode(&frame, &keyframe)?;
          given += 1;
          if given == 1 {
            maybe_first_encode = Some(Instant::now());
          }
        }
        frame.close();
        pipeline.observe_encoders(given, skipped);
      },
      Err(_) => failed_pictures += 1,
    }
    sleep(PERIOD.saturating_sub(picture_started.elapsed())).await?;
  }
  let run_seconds = run_started.elapsed().as_secs_f64();
  JsFuture::from(flush(&encoder)?).await?;
  let _ = encoder.close();
  if let Some(error) = failure.take() {
    return Err(error.into());
  }
  // How long the encoder took for its first chunk: a slow start (the hardware encoder opening)
  // fills its queue, and the pictures meanwhile are skipped.
  let first_chunk_ms = match (maybe_first_encode, first_chunk.get()) {
    (Some(encoded), Some(chunk)) => chunk.duration_since(encoded).as_secs_f64() * 1000.0,
    _ => -1.0,
  };
  let summary = pipeline.summary();
  let summary = format!(
    "{{\"bench\":{{\"frames\":{frames},\"width\":{width},\"height\":{height},\"run_seconds\":{run_seconds:.2},\"worker_start_ms\":{worker_start_ms:.0},\"failed_pictures\":{failed_pictures},\"encoded_chunks\":{},\"first_chunk_ms\":{first_chunk_ms:.0}}},\"effects\":{summary}}}",
    chunks.get(),
  );
  Ok(summary.into())
}

/// An H.264 encoder as a call's (`call_web::video`): annexb, real time, counting its chunks.
fn encoder(
  width: u32,
  height: u32,
  chunks: &Rc<Cell<u32>>,
  first_chunk: &Rc<Cell<Option<Instant>>>,
  failure: &Rc<Cell<Option<String>>>,
) -> Result<VideoEncoder, JsValue> {
  let output = {
    let (chunks, first_chunk) = (Rc::clone(chunks), Rc::clone(first_chunk));
    Closure::<dyn FnMut(JsValue)>::new(move |_chunk: JsValue| {
      if chunks.get() == 0 {
        first_chunk.set(Some(Instant::now()));
      }
      chunks.set(chunks.get() + 1);
    })
  };
  let error = {
    let failure = Rc::clone(failure);
    Closure::<dyn FnMut(JsValue)>::new(move |error: JsValue| failure.set(Some(format!("encoder: {error:?}"))))
  };
  let encoder = VideoEncoder::new(&dictionary(&[
    ("output", output.as_ref().clone()),
    ("error", error.as_ref().clone()),
  ])?)?;
  // The encoder calls them until it is closed, after the run.
  output.forget();
  error.forget();
  encoder.configure(&dictionary(&[
    ("codec", JsValue::from_str("avc1.42e028")),
    ("width", JsValue::from(width)),
    ("height", JsValue::from(height)),
    ("bitrate", JsValue::from(BITRATE_BPS)),
    ("framerate", JsValue::from(30)),
    ("latencyMode", JsValue::from_str("realtime")),
    (
      "avc",
      dictionary(&[("format", JsValue::from_str("annexb"))])?.into(),
    ),
  ])?)?;
  Ok(encoder)
}

/// `encoder.flush()`, which the bindings don't have.
fn flush(encoder: &VideoEncoder) -> Result<js_sys::Promise, JsValue> {
  let flush: js_sys::Function = Reflect::get(encoder, &"flush".into())?.dyn_into()?;
  Ok(flush.call0(encoder)?.unchecked_into())
}

async fn sleep(duration: Duration) -> Result<(), JsValue> {
  let window = web_sys::window().ok_or("no window")?;
  let millis = duration.as_millis() as i32;
  let promise = js_sys::Promise::new(&mut |resolve, _| {
    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
  });
  JsFuture::from(promise).await.map(|_| ())
}
