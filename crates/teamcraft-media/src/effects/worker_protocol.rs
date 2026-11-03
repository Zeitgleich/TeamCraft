//! The messages between the browser's call module and its segmentation worker (#439).
//!
//! The person segmentation (tract and the model) is a wasm module of its own,
//! `teamcraft-web-segmentation`, which runs in a dedicated worker the call module starts when the
//! first background effect does: the call module, which the service worker warms for every member,
//! stays small, and the segmentation leaves the page's main thread. Messages are plain objects with
//! a [`KIND`]; pictures and masks travel as transferred `ArrayBuffer`s.
//!
//! | From   | `kind`       | Fields                                           | Answer                 |
//! |--------|--------------|--------------------------------------------------|------------------------|
//! | worker | [`READY`]    | –                                                | –                      |
//! | page   | [`LOAD`]     | [`URL`] of the model                             | [`LOADED`] or [`FAILED`] |
//! | page   | [`SEGMENT`]  | [`WIDTH`], [`HEIGHT`], [`PIXELS`] (RGBA bytes)   | [`MASK`] or [`FAILED`] |
//! | worker | [`MASK`]     | [`WIDTH`], [`HEIGHT`], [`VALUES`] (f32), [`MILLISECONDS`] | –             |
//! | worker | [`FAILED`]   | [`DETAIL`]                                       | –                      |
//!
//! The worker answers one message at a time, in order. The page sends the next picture only once
//! the last mask is back, and reuses that mask meanwhile.
//!
//! The page finds the worker's script and the model in `<meta>` tags of the build
//! ([`worker_url`], [`model_url`]).

use super::EffectsError;

/// The message's type.
pub const KIND: &str = "kind";
/// The worker's module is initialized and takes messages.
pub const READY: &str = "ready";
/// Load the model from [`URL`] (a path of the page's origin), check it against the pinned SHA-256
/// and parse it.
pub const LOAD: &str = "load";
/// The model is loaded.
pub const LOADED: &str = "loaded";
/// Segment the picture in [`PIXELS`].
pub const SEGMENT: &str = "segment";
/// The person mask of the last picture.
pub const MASK: &str = "mask";
/// The last request failed (or the worker's module couldn't start), with a [`DETAIL`].
pub const FAILED: &str = "failed";

pub const URL: &str = "url";
pub const WIDTH: &str = "width";
pub const HEIGHT: &str = "height";
/// RGBA bytes, row by row.
pub const PIXELS: &str = "pixels";
/// Person probabilities (0..1) as `f32`, row by row.
pub const VALUES: &str = "values";
/// How long the segmentation took in the worker.
pub const MILLISECONDS: &str = "ms";
pub const DETAIL: &str = "detail";

/// Where the page serves the segmentation model: the `content` of its `<meta>`. Only a path of the
/// page's own origin (its Content-Security-Policy allows no other).
pub fn model_url(maybe_meta: Option<&str>) -> Result<String, EffectsError> {
  same_origin_path(maybe_meta, ".onnx", "segmentation model")
}

/// Where the page serves the segmentation worker's script: the `content` of its `<meta>`. Only a
/// script of the page's own origin.
pub fn worker_url(maybe_meta: Option<&str>) -> Result<String, EffectsError> {
  same_origin_path(maybe_meta, ".js", "segmentation worker")
}

fn same_origin_path(maybe_meta: Option<&str>, suffix: &str, what: &str) -> Result<String, EffectsError> {
  let path = maybe_meta
    .map(str::trim)
    .filter(|path| !path.is_empty())
    .ok_or_else(|| EffectsError::Segmentation(format!("this build serves no {what}")))?;
  if !path.starts_with('/') || path.starts_with("//") || !path.ends_with(suffix) {
    return Err(EffectsError::Segmentation(format!(
      "{path} is no {what} path of this origin"
    )));
  }
  Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_model_comes_from_a_path_of_the_page() {
    assert_eq!(
      model_url(Some("/models/selfie_segmentation_landscape-0123.onnx")).unwrap(),
      "/models/selfie_segmentation_landscape-0123.onnx"
    );
    for refused in [
      None,
      Some(""),
      Some("  "),
      Some("https://elsewhere.example.com/model.onnx"),
      Some("//elsewhere.example.com/model.onnx"),
      Some("/models/model.wasm"),
    ] {
      assert!(model_url(refused).is_err(), "{refused:?}");
    }
  }

  #[test]
  fn the_worker_is_a_script_of_the_page() {
    assert_eq!(
      worker_url(Some("/teamcraft-segmentation-worker-0123.js")).unwrap(),
      "/teamcraft-segmentation-worker-0123.js"
    );
    for refused in [
      None,
      Some(""),
      Some("https://elsewhere.example.com/worker.js"),
      Some("//elsewhere.example.com/worker.js"),
      Some("/teamcraft-web-segmentation_bg.wasm"),
    ] {
      assert!(worker_url(refused).is_err(), "{refused:?}");
    }
  }
}
