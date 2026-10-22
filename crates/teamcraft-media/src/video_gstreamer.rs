//! Hardware H.264 encoding through GStreamer: VA-API (`vah264enc`) on Linux, VideoToolbox
//! (`vtenc_h264`) on macOS; V4L2 encoders (`v4l2h264enc`) on ARM boards.
//!
//! GStreamer is LGPL and linked dynamically from the system; the Rust bindings are safe. One
//! pipeline per layer: `appsrc` (NV12 frames) → hardware encoder → Constrained Baseline caps →
//! `h264parse` (SPS/PPS before every keyframe) → `appsink` (Annex B access units). Encoders that
//! can't be built (no plugin, no GPU, unsupported size) return `None`, and [`FallbackEncoder`]
//! switches to OpenH264 if one fails later.
//!
//! [`FallbackEncoder`]: crate::video_codec::FallbackEncoder

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};
use teamcraft_proto::{VideoCodec, VideoLayerInfo};
use tracing::{debug, info};

use crate::video::{VideoFrame, VideoResolution};
use crate::video_codec::{EncodedPicture, VideoCodecError, VideoContent, VideoEncoder, nv12_bytes};

/// Keyframe distance in frames, like OpenH264's.
const KEYFRAME_INTERVAL_FRAMES: u32 = 300;
/// How long one frame may take through the encoder before it counts as dropped.
const PULL_TIMEOUT_MS: u64 = 50;

/// A hardware H.264 encoder for one layer.
pub struct GstreamerEncoder {
  pipeline: gst::Pipeline,
  source: AppSrc,
  sink: AppSink,
  resolution: VideoResolution,
}

/// The platform's hardware encoder element and its settings.
struct HardwareElement {
  name: &'static str,
  /// `gst-launch` style properties for the layer's bitrate (kbit/s).
  properties: fn(bitrate_kbps: u32) -> String,
}

impl GstreamerEncoder {
  /// The first of the platform's hardware encoders that works for `layer`; `None` when GStreamer,
  /// the elements or the hardware aren't available.
  pub fn new(layer: &VideoLayerInfo, content: VideoContent) -> Option<Self> {
    if HARDWARE_ELEMENTS.is_empty() {
      return None;
    }
    if let Err(error) = gst::init() {
      info!(%error, "GStreamer is not available; encoding in software");
      return None;
    }
    let bitrate_kbps = (layer.max_bitrate_bps / 1000).max(1);
    for element in HARDWARE_ELEMENTS {
      if gst::ElementFactory::find(element.name).is_none() {
        debug!(element = element.name, "hardware encoder element not installed");
        continue;
      }
      let description = format!(
        "appsrc name=source is-live=true format=time ! {} {} ! \
         video/x-h264,profile=constrained-baseline,stream-format=byte-stream,alignment=au ! \
         h264parse config-interval=-1 ! appsink name=sink sync=false",
        element.name,
        (element.properties)(bitrate_kbps),
      );
      match Self::build(&description, layer, content) {
        Ok(encoder) => return Some(encoder),
        Err(error) => info!(%error, element = element.name, "hardware encoder unusable"),
      }
    }
    info!("no usable hardware H.264 encoder; encoding in software");
    None
  }

  fn build(
    description: &str,
    layer: &VideoLayerInfo,
    content: VideoContent,
  ) -> Result<Self, VideoCodecError> {
    let pipeline = gst::parse::launch(description)
      .map_err(gstreamer_error)?
      .downcast::<gst::Pipeline>()
      .map_err(|_| VideoCodecError::Codec("not a pipeline".into()))?;
    let source = pipeline
      .by_name("source")
      .and_then(|element| element.downcast::<AppSrc>().ok())
      .ok_or_else(|| VideoCodecError::Codec("no appsrc".into()))?;
    let sink = pipeline
      .by_name("sink")
      .and_then(|element| element.downcast::<AppSink>().ok())
      .ok_or_else(|| VideoCodecError::Codec("no appsink".into()))?;
    let framerate = i32::from(layer.max_framerate.max(1));
    let caps = gst::Caps::builder("video/x-raw")
      .field("format", "NV12")
      .field("width", i32::from(layer.width))
      .field("height", i32::from(layer.height))
      .field("framerate", gst::Fraction::new(framerate, 1))
      .build();
    source.set_caps(Some(&caps));
    debug!(
      ?content,
      width = layer.width,
      height = layer.height,
      "starting the hardware encoder"
    );
    pipeline.set_state(gst::State::Playing).map_err(gstreamer_error)?;
    Ok(Self {
      pipeline,
      source,
      sink,
      resolution: VideoResolution {
        width: layer.width,
        height: layer.height,
      },
    })
  }

  /// The pipeline's first error message, if it reported one.
  fn bus_error(&self) -> Option<String> {
    let bus = self.pipeline.bus()?;
    bus
      .pop_filtered(&[gst::MessageType::Error])
      .and_then(|message| match message.view() {
        gst::MessageView::Error(error) => Some(error.error().to_string()),
        _ => None,
      })
  }
}

impl VideoEncoder for GstreamerEncoder {
  fn codec(&self) -> VideoCodec {
    VideoCodec::H264
  }

  fn encode(
    &mut self,
    frame: &VideoFrame,
    is_keyframe_forced: bool,
  ) -> Result<Option<EncodedPicture>, VideoCodecError> {
    let frame = frame.scaled(self.resolution)?;
    if is_keyframe_forced {
      let event = gstreamer_video::DownstreamForceKeyUnitEvent::builder()
        .all_headers(true)
        .build();
      self.source.send_event(event);
    }
    let width = usize::from(self.resolution.width);
    let height = usize::from(self.resolution.height);
    let mut buffer = gst::Buffer::from_mut_slice(nv12_bytes(&frame, width, height));
    buffer
      .make_mut()
      .set_pts(gst::ClockTime::from_useconds(frame.timestamp_us()));
    self.source.push_buffer(buffer).map_err(gstreamer_error)?;
    let Some(sample) = self
      .sink
      .try_pull_sample(gst::ClockTime::from_mseconds(PULL_TIMEOUT_MS))
    else {
      if let Some(error) = self.bus_error() {
        return Err(VideoCodecError::Codec(error));
      }
      return Ok(None);
    };
    let buffer = sample
      .buffer()
      .ok_or_else(|| VideoCodecError::Codec("sample without buffer".into()))?;
    let is_keyframe = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
    let map = buffer.map_readable().map_err(gstreamer_error)?;
    Ok(Some(EncodedPicture {
      is_keyframe,
      data: map.as_slice().to_vec(),
    }))
  }
}

impl Drop for GstreamerEncoder {
  fn drop(&mut self) {
    // Tearing down: a failing state change leaves nothing else to do.
    let _ = self.pipeline.set_state(gst::State::Null);
  }
}

/// Linux: VA-API (AMD and Intel GPUs, mostly x86_64), then V4L2 memory-to-memory encoders (the
/// hardware encoders of ARM boards and SoCs, e.g. Raspberry Pi 4, Rockchip).
#[cfg(target_os = "linux")]
const HARDWARE_ELEMENTS: &[HardwareElement] = &[
  HardwareElement {
    name: "vah264enc",
    properties: |bitrate_kbps| {
      format!(
        "bitrate={bitrate_kbps} rate-control=cbr key-int-max={KEYFRAME_INTERVAL_FRAMES} b-frames=0 \
         ref-frames=1 cabac=false"
      )
    },
  },
  HardwareElement {
    name: "v4l2h264enc",
    properties: |bitrate_kbps| {
      let bitrate_bps = u64::from(bitrate_kbps) * 1000;
      format!(
        "extra-controls=\"controls,video_bitrate={bitrate_bps},h264_i_frame_period={KEYFRAME_INTERVAL_FRAMES}\""
      )
    },
  },
];

// Compiles for macOS; untested (no Mac in development so far).
#[cfg(target_os = "macos")]
const HARDWARE_ELEMENTS: &[HardwareElement] = &[HardwareElement {
  name: "vtenc_h264",
  properties: |bitrate_kbps| {
    format!(
      "bitrate={bitrate_kbps} realtime=true allow-frame-reordering=false \
       max-keyframe-interval={KEYFRAME_INTERVAL_FRAMES}"
    )
  },
}];

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const HARDWARE_ELEMENTS: &[HardwareElement] = &[];

fn gstreamer_error(error: impl std::fmt::Debug) -> VideoCodecError {
  VideoCodecError::Codec(format!("GStreamer: {error:?}"))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::video::simulcast_layers;
  use crate::video_capture::test_pattern;
  use crate::video_codec::{OpenH264Decoder, VideoDecoder};

  const HD: VideoResolution = VideoResolution {
    width: 1280,
    height: 720,
  };

  /// Runs where a hardware encoder exists; skips elsewhere (CI runners have no GPU) unless
  /// `TEAMCRAFT_REQUIRE_HARDWARE_ENCODER` is set.
  #[test]
  #[cfg_attr(
    not(feature = "openh264-source"),
    ignore = "decodes with OpenH264: --features openh264-source"
  )]
  fn hardware_pictures_decode_and_keyframes_come_on_request() {
    let layer = simulcast_layers(HD, 30, 2_000_000, 1)[0];
    let Some(mut encoder) = GstreamerEncoder::new(&layer, VideoContent::Camera) else {
      assert!(
        std::env::var_os("TEAMCRAFT_REQUIRE_HARDWARE_ENCODER").is_none(),
        "no hardware encoder"
      );
      return;
    };
    let mut decoder = OpenH264Decoder::new().unwrap();
    let mut pictures = Vec::new();
    for index in 0..40u64 {
      let frame = test_pattern(HD, index, index * 33_333).unwrap();
      if let Some(picture) = encoder.encode(&frame, index == 20).unwrap() {
        pictures.push((index, picture));
      }
    }
    assert!(
      pictures.len() >= 35,
      "only {} of 40 frames came out",
      pictures.len()
    );
    assert!(pictures[0].1.is_keyframe, "the stream starts with a keyframe");
    let keyframes: Vec<u64> = pictures
      .iter()
      .filter(|(_, picture)| picture.is_keyframe)
      .map(|(index, _)| *index)
      .collect();
    assert!(
      keyframes.iter().any(|index| (20..=22).contains(index)),
      "forced keyframe missing: {keyframes:?}"
    );
    let mut decoded = 0;
    for (index, picture) in &pictures {
      if let Some(frame) = decoder.decode(&picture.data, index * 33_333).unwrap() {
        assert_eq!(frame.resolution(), HD);
        decoded += 1;
      }
    }
    assert!(
      decoded + 2 >= pictures.len(),
      "decoded {decoded} of {}",
      pictures.len()
    );
  }
}
