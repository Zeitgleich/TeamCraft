//! Background effects on the sending device, before encoding (PLAN.md §8): blur (light or strong)
//! or a background image behind the person a segmentation model finds.
//!
//! The shared part, [`Compositor`], is WGSL on wgpu, so desktop and browser (WebGPU) look the
//! same: the mask is smoothed over time (no flicker), feathered at the edges, and the background
//! is blurred at a quarter of the resolution or replaced by an image scaled to cover the frame.
//! Segmentation is a [`PersonSegmenter`]; platforms with their own (Apple Vision, ML Kit) plug in
//! there.

mod compositor;
mod presets;
mod segmentation;

use std::sync::Arc;

pub use compositor::Compositor;
pub use presets::{PRESET_HEIGHT, PRESET_WIDTH, PresetBackground};
pub use segmentation::{EllipseSegmenter, PersonSegmenter, SegmentationMask};
#[cfg(feature = "segmentation")]
pub use segmentation::{SegmentationModel, TensorLayout, TractSegmenter};
use thiserror::Error;

use crate::video::{VideoError, VideoFrame};

/// What happens behind the person.
#[derive(Clone, Debug, PartialEq)]
pub enum BackgroundEffect {
  None,
  Blur(BlurStrength),
  /// Replaces the background with this image, scaled to cover the frame.
  Image(Arc<BackgroundImage>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlurStrength {
  Light,
  Strong,
}

/// An RGBA8 image (a preset or the user's own, decoded by the app).
#[derive(Clone, PartialEq, Eq)]
pub struct BackgroundImage {
  width: u32,
  height: u32,
  rgba: Vec<u8>,
}

/// Tuning of the mask; the defaults suit a 256x144 segmentation of a 720p camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectsConfig {
  /// Weight of the newest mask against the smoothed history (1: no smoothing).
  pub mask_smoothing: f32,
  /// Feather radius at the mask's resolution.
  pub feather_radius: u32,
  /// Mask values mapped to 0 and 1 (`smoothstep`), which sharpens or softens the edge.
  pub edge_low: f32,
  pub edge_high: f32,
}

/// Segmentation plus compositing, for the encoder thread's frame filter.
pub struct EffectsPipeline {
  segmenter: Box<dyn PersonSegmenter>,
  compositor: Compositor,
  effect: BackgroundEffect,
}

#[derive(Debug, Error)]
pub enum EffectsError {
  #[error("no GPU adapter for effects")]
  NoAdapter,
  #[error("GPU: {0}")]
  Gpu(String),
  #[error("frame width {0} is not a multiple of 8")]
  UnsupportedWidth(u16),
  #[error("background image is {width}x{height} with {bytes} bytes, expected RGBA8")]
  InvalidImage { width: u32, height: u32, bytes: usize },
  #[error("segmentation: {0}")]
  Segmentation(String),
  #[error(transparent)]
  Frame(#[from] VideoError),
}

impl BackgroundImage {
  pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, EffectsError> {
    if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
      return Err(EffectsError::InvalidImage {
        width,
        height,
        bytes: rgba.len(),
      });
    }
    Ok(Self { width, height, rgba })
  }

  pub fn width(&self) -> u32 {
    self.width
  }

  pub fn height(&self) -> u32 {
    self.height
  }

  pub fn rgba(&self) -> &[u8] {
    &self.rgba
  }
}

impl Default for EffectsConfig {
  fn default() -> Self {
    Self {
      mask_smoothing: 0.6,
      feather_radius: 3,
      edge_low: 0.25,
      edge_high: 0.75,
    }
  }
}

impl EffectsPipeline {
  pub fn new(segmenter: Box<dyn PersonSegmenter>, compositor: Compositor, effect: BackgroundEffect) -> Self {
    Self {
      segmenter,
      compositor,
      effect,
    }
  }

  pub fn set_effect(&mut self, effect: BackgroundEffect) {
    self.effect = effect;
  }

  /// The frame with the effect applied; unchanged when the effect is `None`.
  pub async fn apply(&mut self, frame: VideoFrame) -> Result<VideoFrame, EffectsError> {
    if self.effect == BackgroundEffect::None {
      return Ok(frame);
    }
    let mask = self.segmenter.segment(&frame)?;
    self.compositor.apply(&frame, &mask, &self.effect).await
  }

  /// [`EffectsPipeline::apply`] for synchronous callers (the native encoder thread).
  #[cfg(not(target_arch = "wasm32"))]
  pub fn apply_blocking(&mut self, frame: VideoFrame) -> Result<VideoFrame, EffectsError> {
    pollster::block_on(self.apply(frame))
  }
}

/// Changes the effect of a running [`EffectsPipeline::into_frame_filter`] (e.g. from the UI).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct EffectHandle(Arc<std::sync::Mutex<BackgroundEffect>>);

#[cfg(not(target_arch = "wasm32"))]
impl EffectHandle {
  pub fn set(&self, effect: BackgroundEffect) {
    *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = effect;
  }

  fn get(&self) -> BackgroundEffect {
    self
      .0
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
  }
}

#[cfg(not(target_arch = "wasm32"))]
impl EffectsPipeline {
  /// A frame filter for the encoder thread (`VideoSendConfig::maybe_filter`) and a handle to
  /// change its effect. Frames the effects fail on are sent unchanged (and logged once a second at
  /// most), so a GPU hiccup never stops the video.
  pub fn into_frame_filter(mut self) -> (Box<dyn FnMut(VideoFrame) -> VideoFrame + Send>, EffectHandle) {
    let handle = EffectHandle(Arc::new(std::sync::Mutex::new(self.effect.clone())));
    let filter_handle = handle.clone();
    let mut maybe_last_warning: Option<std::time::Instant> = None;
    let filter = move |frame: VideoFrame| {
      let effect = filter_handle.get();
      if effect != self.effect {
        self.set_effect(effect);
      }
      match self.apply_blocking(frame.clone()) {
        Ok(output) => output,
        Err(error) => {
          if maybe_last_warning.is_none_or(|last| last.elapsed() > std::time::Duration::from_secs(1)) {
            tracing::warn!(%error, "background effect skipped");
            maybe_last_warning = Some(std::time::Instant::now());
          }
          frame
        },
      }
    };
    (Box::new(filter), handle)
  }
}

impl std::fmt::Debug for BackgroundImage {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("BackgroundImage")
      .field("width", &self.width)
      .field("height", &self.height)
      .finish_non_exhaustive()
  }
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use super::*;
  use crate::video::VideoResolution;

  /// Small enough for committed golden files, width a multiple of 8.
  const SIZE: VideoResolution = VideoResolution {
    width: 160,
    height: 96,
  };
  const MASK: EllipseSegmenter = EllipseSegmenter {
    width: 64,
    height: 36,
  };
  /// Set to regenerate the golden images after an intended change.
  const UPDATE_VARIABLE: &str = "TEAMCRAFT_UPDATE_GOLDEN";

  mod golden {
    use super::*;

    #[test]
    fn light_blur() {
      check_golden("blur_light", &BackgroundEffect::Blur(BlurStrength::Light));
    }

    #[test]
    fn strong_blur() {
      check_golden("blur_strong", &BackgroundEffect::Blur(BlurStrength::Strong));
    }

    #[test]
    fn background_image() {
      check_golden("image", &BackgroundEffect::Image(Arc::new(gradient_image())));
    }
  }

  mod behaviour {
    use super::*;

    #[test]
    fn person_stays_and_background_is_replaced() {
      let Some(mut compositor) = compositor(EffectsConfig::default()) else {
        return;
      };
      let frame = input_frame();
      let mask = MASK.clone().segment(&frame).unwrap();
      let effect = BackgroundEffect::Image(Arc::new(solid_image([0, 0, 255])));
      let output = pollster::block_on(compositor.apply(&frame, &mask, &effect)).unwrap();
      let centre = luma(&output, 80, 60);
      assert!(
        (i32::from(centre) - i32::from(luma(&frame, 80, 60))).abs() <= 2,
        "person unchanged"
      );
      // Pure blue in BT.601 limited range has a luma of 41.
      assert!(
        (i32::from(luma(&output, 2, 2)) - 41).abs() <= 2,
        "{}",
        luma(&output, 2, 2)
      );
    }

    #[test]
    fn mask_changes_fade_in_over_frames() {
      let Some(mut compositor) = compositor(EffectsConfig {
        mask_smoothing: 0.5,
        ..EffectsConfig::default()
      }) else {
        return;
      };
      let frame = input_frame();
      let effect = BackgroundEffect::Image(Arc::new(solid_image([0, 0, 0])));
      let everywhere = SegmentationMask::new(8, 6, vec![1.0; 48]).unwrap();
      let nowhere = SegmentationMask::new(8, 6, vec![0.0; 48]).unwrap();
      pollster::block_on(compositor.apply(&frame, &everywhere, &effect)).unwrap();
      let original = i32::from(luma(&frame, 80, 48));
      let mut values = Vec::new();
      for _ in 0..4 {
        let output = pollster::block_on(compositor.apply(&frame, &nowhere, &effect)).unwrap();
        values.push(i32::from(luma(&output, 80, 48)));
      }
      // Black background: luma 16. The history halves each frame (0.5, 0.25, ...), and the edge
      // mapping (0.25..0.75) turns the first step into a blend, later ones into background.
      assert!(values.windows(2).all(|pair| pair[1] <= pair[0]), "{values:?}");
      assert!(
        values[0] > 16 + 10 && values[0] < original - 10,
        "first step blends: {values:?}"
      );
      assert!(values[3] <= 18, "background in the end: {values:?}");
    }

    #[test]
    fn widths_that_are_not_a_multiple_of_8_are_rejected() {
      let Some(mut compositor) = compositor(EffectsConfig::default()) else {
        return;
      };
      let frame = VideoFrame::new(
        VideoResolution {
          width: 162,
          height: 96,
        },
        0,
        vec![128; 162 * 96 * 3 / 2],
      )
      .unwrap();
      let mask = SegmentationMask::new(4, 4, vec![1.0; 16]).unwrap();
      let effect = BackgroundEffect::Blur(BlurStrength::Light);
      assert!(matches!(
        pollster::block_on(compositor.apply(&frame, &mask, &effect)),
        Err(EffectsError::UnsupportedWidth(162))
      ));
    }

    #[test]
    fn no_effect_passes_the_frame_through() {
      let Some(mut compositor) = compositor(EffectsConfig::default()) else {
        return;
      };
      let frame = input_frame();
      let mask = MASK.clone().segment(&frame).unwrap();
      let output = pollster::block_on(compositor.apply(&frame, &mask, &BackgroundEffect::None)).unwrap();
      assert_eq!(output, frame);
    }

    #[test]
    fn frame_filter_follows_its_handle() {
      let Some(compositor) = compositor(EffectsConfig::default()) else {
        return;
      };
      let pipeline = EffectsPipeline::new(Box::new(MASK), compositor, BackgroundEffect::None);
      let (mut filter, handle) = pipeline.into_frame_filter();
      let frame = input_frame();
      assert_eq!(filter(frame.clone()), frame, "no effect yet");
      handle.set(BackgroundEffect::Blur(BlurStrength::Strong));
      assert_ne!(filter(frame.clone()), frame);
      handle.set(BackgroundEffect::None);
      assert_eq!(filter(frame.clone()), frame);
    }

    #[test]
    fn invalid_inputs_are_rejected() {
      assert!(BackgroundImage::new(2, 2, vec![0; 15]).is_err());
      assert!(SegmentationMask::new(2, 2, vec![0.0; 3]).is_err());
    }
  }

  /// Runs `effect` on the test frame and compares with `golden/{name}.i420` (mean absolute
  /// difference ≤ 1.5, no pixel off by more than 24: GPUs round differently).
  fn check_golden(name: &str, effect: &BackgroundEffect) {
    let Some(mut compositor) = compositor(EffectsConfig::default()) else {
      return;
    };
    let frame = input_frame();
    let mask = MASK.clone().segment(&frame).unwrap();
    let output = pollster::block_on(compositor.apply(&frame, &mask, effect)).unwrap();
    let path = golden_path(name);
    if std::env::var_os(UPDATE_VARIABLE).is_some() {
      std::fs::write(&path, output.data()).unwrap();
      return;
    }
    let golden = std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert_eq!(golden.len(), output.data().len());
    let differences: Vec<u32> = golden
      .iter()
      .zip(output.data())
      .map(|(a, b)| u32::from(a.abs_diff(*b)))
      .collect();
    let mean = differences.iter().sum::<u32>() as f64 / differences.len() as f64;
    let max = differences.iter().copied().max().unwrap_or(0);
    assert!(mean <= 1.5 && max <= 24, "{name}: mean {mean:.2}, max {max}");
    assert_ne!(output.data(), frame.data(), "{name} changed nothing");
  }

  /// `None` (test skipped) when the machine has no GPU adapter at all.
  fn compositor(config: EffectsConfig) -> Option<Compositor> {
    match pollster::block_on(Compositor::new(config)) {
      Ok(compositor) => Some(compositor),
      Err(EffectsError::NoAdapter) => {
        eprintln!("skipping: no GPU adapter");
        None
      },
      Err(error) => panic!("{error}"),
    }
  }

  fn input_frame() -> VideoFrame {
    // Diagonal stripes in luma, a horizontal chroma ramp: blur and replacement both show.
    let (width, height) = (usize::from(SIZE.width), usize::from(SIZE.height));
    let mut data = Vec::with_capacity(width * height * 3 / 2);
    for y in 0..height {
      for x in 0..width {
        data.push(if (x + y) / 6 % 2 == 0 { 200 } else { 50 });
      }
    }
    for plane in [0u8, 1] {
      for _ in 0..height / 2 {
        for x in 0..width / 2 {
          let ramp = (x * 255 / (width / 2)) as u8;
          data.push(if plane == 0 { ramp } else { 255 - ramp });
        }
      }
    }
    VideoFrame::new(SIZE, 0, data).unwrap()
  }

  fn gradient_image() -> BackgroundImage {
    let (width, height) = (40u32, 30u32);
    let rgba = (0..height)
      .flat_map(|y| (0..width).flat_map(move |x| [(x * 6) as u8, (y * 8) as u8, 120, 255]))
      .collect();
    BackgroundImage::new(width, height, rgba).unwrap()
  }

  fn solid_image(rgb: [u8; 3]) -> BackgroundImage {
    BackgroundImage::new(4, 4, [rgb[0], rgb[1], rgb[2], 255].repeat(16)).unwrap()
  }

  fn luma(frame: &VideoFrame, x: usize, y: usize) -> u8 {
    frame.planes().0[y * usize::from(frame.resolution().width) + x]
  }

  fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
      .join("tests/golden")
      .join(format!("{name}.i420"))
  }
}
