//! The bundled segmentation model (`models/fetch.sh`) loads and segments a frame. Skipped when
//! the model hasn't been fetched (it is not in git).

#![cfg(all(feature = "segmentation", feature = "native"))]
// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use teamcraft_media::effects::{PersonSegmenter, SegmentationModel, TractSegmenter};
use teamcraft_media::video::VideoResolution;
use teamcraft_media::video_capture::test_pattern;

#[test]
fn the_bundled_model_segments_frames() {
  let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models/selfie_segmentation_landscape.onnx");
  let Ok(onnx) = std::fs::read(&path) else {
    return;
  };
  let mut segmenter = TractSegmenter::new(&onnx, SegmentationModel::MEDIAPIPE_LANDSCAPE).unwrap();
  let frame = test_pattern(
    VideoResolution {
      width: 1280,
      height: 720,
    },
    0,
    0,
  )
  .unwrap();
  let mask = segmenter.segment(&frame).unwrap();
  assert_eq!((mask.width(), mask.height()), (256, 144));
  assert!(mask.values().iter().all(|value| (0.0..=1.0).contains(value)));
}
