//! Who is the person in the frame: a low-resolution probability mask per frame.
//!
//! [`TractSegmenter`] runs a selfie segmentation model (MediaPipe's, Apache-2.0, converted to
//! ONNX) with tract on the CPU, natively or in WebAssembly. The apps load the ONNX file; the
//! browser loads the same graph as NNEF, tract's own format ([`onnx_to_nnef`]), which needs no
//! ONNX parser in its worker module (#1295). [`EllipseSegmenter`] is the fake for tests and for
//! checking the compositor without a model.

use crate::video::VideoFrame;

use super::EffectsError;

/// The model's input size (MediaPipe's landscape selfie segmentation). The browser scales its
/// camera picture to it before handing it to the segmentation worker (#439).
pub const MODEL_INPUT_WIDTH: u32 = 256;
pub const MODEL_INPUT_HEIGHT: u32 = 144;

/// Person probability (0..1) per pixel at the segmenter's resolution, row by row.
#[derive(Clone, PartialEq)]
pub struct SegmentationMask {
  width: u32,
  height: u32,
  values: Vec<f32>,
}

/// Finds the person in a frame.
pub trait PersonSegmenter: Send {
  fn segment(&mut self, frame: &VideoFrame) -> Result<SegmentationMask, EffectsError>;
}

/// A centred ellipse covering the middle of the frame: a stand-in "person".
#[derive(Debug, Clone, Copy)]
pub struct EllipseSegmenter {
  pub width: u32,
  pub height: u32,
}

impl SegmentationMask {
  pub fn new(width: u32, height: u32, values: Vec<f32>) -> Result<Self, EffectsError> {
    if width == 0 || height == 0 || values.len() != width as usize * height as usize {
      return Err(EffectsError::Segmentation(format!(
        "mask of {width}x{height} with {} values",
        values.len()
      )));
    }
    Ok(Self {
      width,
      height,
      values,
    })
  }

  pub fn width(&self) -> u32 {
    self.width
  }

  pub fn height(&self) -> u32 {
    self.height
  }

  pub fn values(&self) -> &[f32] {
    &self.values
  }
}

impl PersonSegmenter for EllipseSegmenter {
  fn segment(&mut self, _frame: &VideoFrame) -> Result<SegmentationMask, EffectsError> {
    let (width, height) = (self.width as f32, self.height as f32);
    let values = (0..self.height)
      .flat_map(|y| {
        (0..self.width).map(move |x| {
          let dx = (x as f32 + 0.5 - width / 2.0) / (width * 0.25);
          let dy = (y as f32 + 0.5 - height * 0.6) / (height * 0.45);
          if dx * dx + dy * dy <= 1.0 { 1.0 } else { 0.0 }
        })
      })
      .collect();
    SegmentationMask::new(self.width, self.height, values)
  }
}

impl std::fmt::Debug for SegmentationMask {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SegmentationMask")
      .field("width", &self.width)
      .field("height", &self.height)
      .finish_non_exhaustive()
  }
}

#[cfg(feature = "segmentation-nnef")]
mod tract {
  use std::sync::Arc;

  use tract_nnef::prelude::*;

  use super::{EffectsError, PersonSegmenter, SegmentationMask};
  use crate::video::VideoFrame;

  type Plan = Arc<TypedRunnableModel>;

  /// How a segmentation model wants its input and gives its output.
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct SegmentationModel {
    /// Input size; the frame is scaled to it.
    pub width: u32,
    pub height: u32,
    /// Output channel with the person probability (the model's only one for MediaPipe's).
    pub person_channel: usize,
    /// Where the channels are in the input and output tensors.
    pub layout: TensorLayout,
  }

  /// Channel order of a model's tensors.
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum TensorLayout {
    /// NHWC, as TensorFlow Lite models (and their direct conversions) use.
    ChannelsLast,
    /// NCHW, as models exported for ONNX runtimes usually use.
    ChannelsFirst,
  }

  /// A selfie segmentation model run with tract: RGB float input in 0..1, person probabilities out.
  /// Copies share the parsed model (the browser parses it once per session).
  #[derive(Clone)]
  pub struct TractSegmenter {
    plan: Plan,
    model: SegmentationModel,
    rgb: Vec<f32>,
  }

  impl SegmentationModel {
    /// MediaPipe selfie segmentation, landscape variant (256x144, one output channel), as the
    /// ONNX export the apps bundle (`models/fetch.sh`): NCHW.
    pub const MEDIAPIPE_LANDSCAPE: Self = Self {
      width: super::MODEL_INPUT_WIDTH,
      height: super::MODEL_INPUT_HEIGHT,
      person_channel: 0,
      layout: TensorLayout::ChannelsFirst,
    };

    fn input_shape(&self) -> [usize; 4] {
      let (width, height) = (self.width as usize, self.height as usize);
      match self.layout {
        TensorLayout::ChannelsLast => [1, height, width, 3],
        TensorLayout::ChannelsFirst => [1, 3, height, width],
      }
    }
  }

  impl TractSegmenter {
    /// Loads an ONNX model (its bytes, e.g. read from the app's data files).
    #[cfg(feature = "segmentation")]
    pub fn from_onnx(onnx: &[u8], model: SegmentationModel) -> Result<Self, EffectsError> {
      let typed = onnx::typed_model(onnx, model).map_err(segmentation_error)?;
      Self::with_typed(typed, model)
    }

    /// Loads the model as NNEF, tract's own format ([`onnx_to_nnef`] made it from the ONNX file):
    /// the browser's segmentation worker reads it without tract's ONNX parser (#1295). Both run
    /// the same graph, so the masks are the same.
    pub fn from_nnef(nnef: &[u8], model: SegmentationModel) -> Result<Self, EffectsError> {
      let typed = nnef_framework()
        .model_for_read(&mut std::io::Cursor::new(nnef))
        .map_err(segmentation_error)?;
      let input = typed.input_fact(0).map_err(segmentation_error)?;
      if input.shape.as_concrete() != Some(&model.input_shape()[..]) {
        return Err(EffectsError::Segmentation(format!(
          "the model takes {:?}, not {:?}",
          input.shape,
          model.input_shape()
        )));
      }
      Self::with_typed(typed, model)
    }

    fn with_typed(typed: TypedModel, model: SegmentationModel) -> Result<Self, EffectsError> {
      let plan = typed
        .into_optimized()
        .and_then(|optimized| optimized.into_runnable())
        .map_err(segmentation_error)?;
      Ok(Self {
        plan,
        model,
        rgb: Vec::new(),
      })
    }
  }

  /// The ONNX model as NNEF (a tar archive) for [`TractSegmenter::from_nnef`]: the same typed graph
  /// [`TractSegmenter::from_onnx`] optimizes, before any optimization, so loading either gives the
  /// same plan. The archive is the same for the same model and tract version (fixed timestamps,
  /// sorted entries): the build pins its SHA-256 like the ONNX file's.
  #[cfg(feature = "segmentation")]
  pub fn onnx_to_nnef(onnx: &[u8], model: SegmentationModel) -> Result<Vec<u8>, EffectsError> {
    let typed = onnx::typed_model(onnx, model).map_err(segmentation_error)?;
    nnef_framework()
      .write_to_tar_with_config(&typed, Vec::new(), false, true)
      .map_err(segmentation_error)
  }

  /// NNEF with tract's core operators and, of its `tract_onnx` extension, only the ONNX operator
  /// the model needs beyond them (Resize): the extension's others and what they bring along
  /// (random numbers, tree ensembles) stay out of the browser's module.
  fn nnef_framework() -> tract_nnef::framework::Nnef {
    let mut onnx = tract_nnef::internal::Registry::new("tract_onnx");
    tract_onnx_opl::resize::register(&mut onnx);
    tract_nnef::nnef().with_registry(onnx)
  }

  #[cfg(feature = "segmentation")]
  mod onnx {
    use std::collections::HashMap;

    use tract_onnx::prelude::*;

    use super::SegmentationModel;

    /// The ONNX model as tract's typed graph, input fixed to `model`'s. Exported models often name
    /// their batch dimension (`batch_size`): it stays symbolic while the graph's facts are
    /// unified, then is fixed to 1 (the frames come one at a time).
    pub(super) fn typed_model(onnx: &[u8], model: SegmentationModel) -> TractResult<TypedModel> {
      let graph = tract_onnx::onnx().model_for_read(&mut std::io::Cursor::new(onnx))?;
      let batch = graph.symbols.sym("batch_size");
      let [_, a, b, c] = model.input_shape().map(TDim::from);
      let fact = InferenceFact::dt_shape(f32::datum_type(), tvec![batch.clone().into(), a, b, c]);
      let typed = graph.with_input_fact(0, fact)?.into_typed()?;
      typed.set_symbols(&HashMap::from([(batch, TDim::from(1))]))
    }
  }

  impl TractSegmenter {
    /// The mask for an RGBA picture (`width` x `height`, row by row), scaled to the model's input
    /// like a frame: the browser's segmentation worker gets the camera picture this way (#439).
    pub fn segment_rgba(
      &mut self,
      width: u32,
      height: u32,
      rgba: &[u8],
    ) -> Result<SegmentationMask, EffectsError> {
      let (source_width, source_height) = (width as usize, height as usize);
      if source_width == 0 || source_height == 0 || rgba.len() != source_width * source_height * 4 {
        return Err(EffectsError::Segmentation(format!(
          "picture of {width}x{height} with {} bytes",
          rgba.len()
        )));
      }
      let (model_width, model_height) = (self.model.width as usize, self.model.height as usize);
      self.rgb.clear();
      self.rgb.reserve(model_width * model_height * 3);
      for row in 0..model_height {
        let source_y = (row * source_height + source_height / 2) / model_height;
        for column in 0..model_width {
          let source_x = (column * source_width + source_width / 2) / model_width;
          let pixel = (source_y * source_width + source_x) * 4;
          for channel in &rgba[pixel..pixel + 3] {
            self.rgb.push(f32::from(*channel) / 255.0);
          }
        }
      }
      to_layout(self.model.layout, &mut self.rgb);
      self.run()
    }

    /// Runs the model on the input in `self.rgb`.
    fn run(&mut self) -> Result<SegmentationMask, EffectsError> {
      let (width, height) = (self.model.width as usize, self.model.height as usize);
      let input = Tensor::from_shape(&self.model.input_shape(), &self.rgb).map_err(segmentation_error)?;
      let outputs = self
        .plan
        .run(tvec!(input.into_tvalue()))
        .map_err(segmentation_error)?;
      let output = outputs
        .first()
        .ok_or_else(|| EffectsError::Segmentation("model has no output".into()))?
        .to_plain_array_view::<f32>()
        .map_err(segmentation_error)?;
      let channels = output.len() / (width * height).max(1);
      if channels == 0 || self.model.person_channel >= channels {
        return Err(EffectsError::Segmentation(format!(
          "output has {channels} channels, person channel is {}",
          self.model.person_channel
        )));
      }
      let values: Vec<f32> = match self.model.layout {
        TensorLayout::ChannelsLast => output
          .iter()
          .skip(self.model.person_channel)
          .step_by(channels)
          .copied()
          .collect(),
        TensorLayout::ChannelsFirst => output
          .iter()
          .skip(self.model.person_channel * width * height)
          .take(width * height)
          .copied()
          .collect(),
      };
      SegmentationMask::new(self.model.width, self.model.height, values)
    }
  }

  impl PersonSegmenter for TractSegmenter {
    fn segment(&mut self, frame: &VideoFrame) -> Result<SegmentationMask, EffectsError> {
      let (width, height) = (self.model.width as usize, self.model.height as usize);
      fill_rgb(frame, width, height, self.model.layout, &mut self.rgb);
      self.run()
    }
  }

  /// Scales the I420 frame to the model size as RGB in 0..1 (nearest sample: the model is far
  /// smaller than the frame), interleaved or as three planes per `layout`.
  fn fill_rgb(frame: &VideoFrame, width: usize, height: usize, layout: TensorLayout, out: &mut Vec<f32>) {
    let resolution = frame.resolution();
    let (frame_width, frame_height) = (usize::from(resolution.width), usize::from(resolution.height));
    let (y_plane, u_plane, v_plane) = frame.planes();
    out.clear();
    out.reserve(width * height * 3);
    for row in 0..height {
      let source_y = (row * frame_height + frame_height / 2) / height;
      for column in 0..width {
        let source_x = (column * frame_width + frame_width / 2) / width;
        let luma = f32::from(y_plane[source_y * frame_width + source_x]) - 16.0;
        let chroma = (source_y / 2) * (frame_width / 2) + source_x / 2;
        let (u, v) = (
          f32::from(u_plane[chroma]) - 128.0,
          f32::from(v_plane[chroma]) - 128.0,
        );
        for value in [
          1.164 * luma + 1.596 * v,
          1.164 * luma - 0.392 * u - 0.813 * v,
          1.164 * luma + 2.017 * u,
        ] {
          out.push((value / 255.0).clamp(0.0, 1.0));
        }
      }
    }
    to_layout(layout, out);
  }

  /// Interleaved RGB as `layout` wants it: three planes for channels first.
  fn to_layout(layout: TensorLayout, rgb: &mut Vec<f32>) {
    if layout == TensorLayout::ChannelsFirst {
      let planar: Vec<f32> = (0..3)
        .flat_map(|channel| rgb.iter().skip(channel).step_by(3).copied().collect::<Vec<_>>())
        .collect();
      *rgb = planar;
    }
  }

  fn segmentation_error(error: impl std::fmt::Display) -> EffectsError {
    EffectsError::Segmentation(format!("{error:#}"))
  }
}

#[cfg(feature = "segmentation")]
pub use tract::onnx_to_nnef;
#[cfg(feature = "segmentation-nnef")]
pub use tract::{SegmentationModel, TensorLayout, TractSegmenter};

#[cfg(all(test, feature = "segmentation"))]
pub(super) mod tests {
  use prost::Message as _;
  use tract_onnx::pb::{
    AttributeProto, GraphProto, ModelProto, NodeProto, OperatorSetIdProto, TypeProto, ValueInfoProto,
    attribute_proto, type_proto,
  };

  use super::*;
  use crate::video::VideoResolution;

  const MODEL: SegmentationModel = SegmentationModel {
    width: 8,
    height: 4,
    person_channel: 0,
    layout: TensorLayout::ChannelsLast,
  };

  #[test]
  fn tract_runs_an_nhwc_model_on_the_scaled_frame() {
    let mut segmenter = TractSegmenter::from_onnx(&mean_of_channels_model(), MODEL).unwrap();
    // Flat grey (Y=U=V=128) is RGB 130/255 in every channel.
    let frame = VideoFrame::new(
      VideoResolution {
        width: 64,
        height: 32,
      },
      0,
      vec![128; 64 * 32 * 3 / 2],
    )
    .unwrap();
    let mask = segmenter.segment(&frame).unwrap();
    assert_eq!((mask.width(), mask.height()), (8, 4));
    assert!(
      mask
        .values()
        .iter()
        .all(|value| (value - 130.4 / 255.0).abs() < 0.01),
      "{mask:?}"
    );
  }

  #[test]
  fn tract_runs_on_an_rgba_picture_scaled_to_the_model() {
    let mut segmenter = TractSegmenter::from_onnx(&mean_of_channels_model(), MODEL).unwrap();
    // Red 60, green 120, blue 180: the model averages the three channels.
    let picture = [60u8, 120, 180, 255].repeat(32 * 16);
    let mask = segmenter.segment_rgba(32, 16, &picture).unwrap();
    assert_eq!((mask.width(), mask.height()), (8, 4));
    assert!(
      mask
        .values()
        .iter()
        .all(|value| (value - 120.0 / 255.0).abs() < 0.001),
      "{mask:?}"
    );
    assert!(
      segmenter.segment_rgba(32, 16, &picture[4..]).is_err(),
      "short picture"
    );
    assert!(segmenter.segment_rgba(0, 16, &[]).is_err(), "empty picture");
  }

  #[test]
  fn the_model_input_size_is_the_landscape_models() {
    let model = SegmentationModel::MEDIAPIPE_LANDSCAPE;
    assert_eq!(
      (model.width, model.height),
      (MODEL_INPUT_WIDTH, MODEL_INPUT_HEIGHT)
    );
  }

  #[test]
  fn broken_models_are_errors() {
    assert!(TractSegmenter::from_onnx(b"not a model", MODEL).is_err());
    assert!(TractSegmenter::from_nnef(b"not a model", MODEL).is_err());
    assert!(onnx_to_nnef(b"not a model", MODEL).is_err());
  }

  #[test]
  fn the_nnef_model_segments_like_the_onnx_one() {
    let onnx = mean_of_channels_model();
    let nnef = onnx_to_nnef(&onnx, MODEL).unwrap();
    let mut from_onnx = TractSegmenter::from_onnx(&onnx, MODEL).unwrap();
    let mut from_nnef = TractSegmenter::from_nnef(&nnef, MODEL).unwrap();
    // A picture with a different colour in every pixel, so a wrong axis or scale shows.
    let picture: Vec<u8> = (0..32u32 * 16)
      .flat_map(|pixel| {
        [
          (pixel % 251) as u8,
          (pixel * 7 % 253) as u8,
          (pixel * 13 % 255) as u8,
          255,
        ]
      })
      .collect();
    let expected = from_onnx.segment_rgba(32, 16, &picture).unwrap();
    assert_eq!(from_nnef.segment_rgba(32, 16, &picture).unwrap(), expected);
    let frame = VideoFrame::new(
      VideoResolution {
        width: 64,
        height: 32,
      },
      0,
      (0..64 * 32 * 3 / 2).map(|index| (index % 241) as u8).collect(),
    )
    .unwrap();
    assert_eq!(
      from_nnef.segment(&frame).unwrap(),
      from_onnx.segment(&frame).unwrap()
    );
  }

  #[test]
  fn an_nnef_model_for_another_input_is_refused() {
    let nnef = onnx_to_nnef(&mean_of_channels_model(), MODEL).unwrap();
    let other = SegmentationModel { width: 16, ..MODEL };
    assert!(TractSegmenter::from_nnef(&nnef, other).is_err());
  }

  /// The browser's model file is pinned by its SHA-256 (`models/*.nnef.sha256`), so the conversion
  /// must give the same archive every time. This pins tract's NNEF writer on a tiny model: when an
  /// upgrade of tract changes it, the browser's model file likely changed too, and
  /// `models/fetch-nnef.sh --pin` makes and pins it anew (after checking its masks, see
  /// `tests/suite/segmentation_model.rs`).
  #[test]
  fn the_nnef_archive_is_the_same_every_time() {
    use sha2::{Digest, Sha256};

    let onnx = mean_of_channels_model();
    let nnef = onnx_to_nnef(&onnx, MODEL).unwrap();
    assert_eq!(onnx_to_nnef(&onnx, MODEL).unwrap(), nnef);
    let sha256: String = Sha256::digest(&nnef)
      .iter()
      .map(|byte| format!("{byte:02x}"))
      .collect();
    assert_eq!(sha256, TEST_MODEL_NNEF_SHA256, "tract's NNEF writer changed");
  }

  /// The SHA-256 of `mean_of_channels_model()` as NNEF (tract 0.23.9).
  const TEST_MODEL_NNEF_SHA256: &str = "56c38d5bebd7be6549a0669e433bc46720302c15271b2ea9a708869f6ddda963";

  /// `mask = ReduceMean(image, axis 3, keepdims)`: NHWC in, one channel out, like MediaPipe's.
  pub(in crate::effects) fn mean_of_channels_model() -> Vec<u8> {
    // Element type 1 is FLOAT; shapes come from the input fact.
    let value = |name: &str| ValueInfoProto {
      name: name.into(),
      r#type: Some(TypeProto {
        value: Some(type_proto::Value::TensorType(type_proto::Tensor {
          elem_type: 1,
          shape: None,
        })),
        ..Default::default()
      }),
      ..Default::default()
    };
    let node = NodeProto {
      input: vec!["image".into()],
      output: vec!["mask".into()],
      op_type: "ReduceMean".into(),
      attribute: vec![
        AttributeProto {
          name: "axes".into(),
          r#type: attribute_proto::AttributeType::Ints as i32,
          ints: vec![3],
          ..Default::default()
        },
        AttributeProto {
          name: "keepdims".into(),
          r#type: attribute_proto::AttributeType::Int as i32,
          i: 1,
          ..Default::default()
        },
      ],
      ..Default::default()
    };
    ModelProto {
      ir_version: 7,
      opset_import: vec![OperatorSetIdProto {
        domain: String::new(),
        version: 13,
      }],
      graph: Some(GraphProto {
        node: vec![node],
        name: "test".into(),
        input: vec![value("image")],
        output: vec![value("mask")],
        ..Default::default()
      }),
      ..Default::default()
    }
    .encode_to_vec()
  }
}
