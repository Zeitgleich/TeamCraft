//! The bundled segmentation model (`models/fetch.sh`) loads and segments a frame, and its NNEF
//! conversion for the browser (`models/fetch-nnef.sh`, #1295) is the pinned file and segments the
//! same. Skipped when the model hasn't been fetched (it is not in git).

#![cfg(all(feature = "segmentation", feature = "native"))]
// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use teamcraft_media::effects::{
  PersonSegmenter, SegmentationModel, TractSegmenter, check_segmentation_model, onnx_to_nnef,
};
use teamcraft_media::video::{VideoFrame, VideoResolution};
use teamcraft_media::video_capture::test_pattern;

#[test]
fn the_bundled_model_segments_frames() {
  let Some(onnx) = bundled_model() else {
    return;
  };
  let mut segmenter = TractSegmenter::from_onnx(&onnx, SegmentationModel::MEDIAPIPE_LANDSCAPE).unwrap();
  let mask = segmenter.segment(&camera_frame(0)).unwrap();
  assert_eq!((mask.width(), mask.height()), (256, 144));
  assert!(mask.values().iter().all(|value| (0.0..=1.0).contains(value)));
}

/// The browser's model is the ONNX model's graph as NNEF: the same masks, value for value, from the
/// frames the apps segment and from the RGBA pictures the browser's worker gets.
#[test]
fn the_nnef_model_segments_like_the_bundled_one() {
  let Some(onnx) = bundled_model() else {
    return;
  };
  let model = SegmentationModel::MEDIAPIPE_LANDSCAPE;
  let nnef = onnx_to_nnef(&onnx, model).unwrap();
  check_segmentation_model(&nnef)
    .expect("the conversion is the pinned NNEF file (models/fetch-nnef.sh --pin)");
  let mut from_onnx = TractSegmenter::from_onnx(&onnx, model).unwrap();
  let mut from_nnef = TractSegmenter::from_nnef(&nnef, model).unwrap();
  let mut masks = Vec::new();
  for moment in [0, 15, 40] {
    let frame = camera_frame(moment);
    let expected = from_onnx.segment(&frame).unwrap();
    assert_eq!(from_nnef.segment(&frame).unwrap(), expected, "frame {moment}");
    masks.push(expected);
  }
  let picture = person_like_picture(model.width, model.height);
  let expected = from_onnx
    .segment_rgba(model.width, model.height, &picture)
    .unwrap();
  assert_eq!(
    from_nnef
      .segment_rgba(model.width, model.height, &picture)
      .unwrap(),
    expected
  );
  assert_ne!(masks[0], expected, "the model sees a difference at all");
}

fn bundled_model() -> Option<Vec<u8>> {
  let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models/selfie_segmentation_landscape.onnx");
  std::fs::read(path).ok()
}

fn camera_frame(moment: u64) -> VideoFrame {
  test_pattern(
    VideoResolution {
      width: 1280,
      height: 720,
    },
    moment,
    moment * 33_333,
  )
  .unwrap()
}

/// A skin-coloured head and shoulders in front of a grey-blue wall, as RGBA.
fn person_like_picture(width: u32, height: u32) -> Vec<u8> {
  let (width, height) = (width as f32, height as f32);
  let mut rgba = Vec::new();
  for y in 0..height as u32 {
    for x in 0..width as u32 {
      let (x, y) = (x as f32 + 0.5, y as f32 + 0.5);
      let head =
        ((x - width * 0.5) / (width * 0.12)).powi(2) + ((y - height * 0.35) / (height * 0.22)).powi(2);
      let body =
        ((x - width * 0.5) / (width * 0.3)).powi(2) + ((y - height * 1.05) / (height * 0.45)).powi(2);
      let pixel = if head <= 1.0 {
        [224, 172, 140, 255]
      } else if body <= 1.0 {
        [40, 60, 120, 255]
      } else {
        [150, 160, 170, 255]
      };
      rgba.extend_from_slice(&pixel);
    }
  }
  rgba
}
