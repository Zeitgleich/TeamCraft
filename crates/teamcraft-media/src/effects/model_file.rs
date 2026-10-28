//! The segmentation model as a file the browser downloads (#439). The web build serves it under a
//! name with its hash (`scripts/finish-calls.sh`), so it is cached for good; the segmentation
//! worker checks it against the SHA-256 it was built with (the pin `models/fetch.sh` checks too),
//! so a server can't hand it another model, then parses it once and keeps it for the session.

use sha2::{Digest, Sha256};

use super::{EffectsError, SegmentationModel, TractSegmenter};

/// The model's file name (`crates/teamcraft-media/models`).
pub const SEGMENTATION_MODEL_FILE: &str = "selfie_segmentation_landscape.onnx";
/// `<sha256 hex>  <file name>`, as `fetch.sh` reads it.
const PINNED_SHA256: &str = include_str!("../../models/selfie_segmentation_landscape.onnx.sha256");

/// The model, checked and parsed once: later starts of the effects get a copy at once. A load that
/// failed isn't kept; the next start tries again.
pub struct ModelCache {
  expected_sha256: String,
  model: SegmentationModel,
  maybe_segmenter: Option<TractSegmenter>,
}

/// Whether `model` is the pinned segmentation model.
pub fn check_segmentation_model(model: &[u8]) -> Result<(), EffectsError> {
  check_against(model, pinned_sha256())
}

impl ModelCache {
  /// For the pinned model (MediaPipe's landscape selfie segmentation).
  pub fn pinned() -> Self {
    Self::new(pinned_sha256(), SegmentationModel::MEDIAPIPE_LANDSCAPE)
  }

  fn new(expected_sha256: &str, model: SegmentationModel) -> Self {
    Self {
      expected_sha256: expected_sha256.to_owned(),
      model,
      maybe_segmenter: None,
    }
  }

  /// A segmenter from the model loaded before, if one was.
  pub fn cached(&self) -> Option<TractSegmenter> {
    self.maybe_segmenter.clone()
  }

  /// Checks and parses the downloaded `file`, keeps it and returns a segmenter for it.
  pub fn store(&mut self, file: &[u8]) -> Result<TractSegmenter, EffectsError> {
    check_against(file, &self.expected_sha256)?;
    let segmenter = TractSegmenter::new(file, self.model)?;
    self.maybe_segmenter = Some(segmenter.clone());
    Ok(segmenter)
  }
}

fn pinned_sha256() -> &'static str {
  PINNED_SHA256.split_whitespace().next().unwrap_or_default()
}

fn check_against(file: &[u8], expected_hex: &str) -> Result<(), EffectsError> {
  let actual: String = Sha256::digest(file)
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect();
  if actual.eq_ignore_ascii_case(expected_hex) {
    Ok(())
  } else {
    Err(EffectsError::Segmentation(format!(
      "the model's SHA-256 is {actual}, not the pinned {expected_hex}"
    )))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::effects::TensorLayout;
  use crate::effects::segmentation::tests::mean_of_channels_model;

  const TEST_MODEL: SegmentationModel = SegmentationModel {
    width: 8,
    height: 4,
    person_channel: 0,
    layout: TensorLayout::ChannelsLast,
  };

  #[test]
  fn the_pin_is_the_one_fetch_sh_checks() {
    assert_eq!(pinned_sha256().len(), 64);
    assert!(pinned_sha256().bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(PINNED_SHA256.trim_end().ends_with(SEGMENTATION_MODEL_FILE));
    assert!(check_segmentation_model(b"not the model").is_err());
  }

  #[test]
  fn a_checked_model_is_parsed_once_and_kept() {
    let file = mean_of_channels_model();
    let mut cache = ModelCache::new(&sha256_hex(&file), TEST_MODEL);
    assert!(cache.cached().is_none());
    cache.store(&file).unwrap();
    assert!(cache.cached().is_some(), "the next start needs no download");
  }

  #[test]
  fn another_file_is_refused_and_not_kept() {
    let file = mean_of_channels_model();
    let mut cache = ModelCache::new(&sha256_hex(b"the pinned model"), TEST_MODEL);
    assert!(matches!(cache.store(&file), Err(EffectsError::Segmentation(_))));
    assert!(cache.cached().is_none(), "the next start tries again");
    // The right hash, but no model: refused by the parser, not kept either.
    let mut broken = ModelCache::new(&sha256_hex(b"garbage"), TEST_MODEL);
    assert!(broken.store(b"garbage").is_err());
    assert!(broken.cached().is_none());
  }

  fn sha256_hex(file: &[u8]) -> String {
    Sha256::digest(file)
      .iter()
      .map(|byte| format!("{byte:02x}"))
      .collect()
  }
}
