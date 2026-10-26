//! Hardware H.264 encoding through GStreamer: VA-API (`vah264enc`) on Linux, VideoToolbox
//! (`vtenc_h264`) on macOS, Media Foundation (`mfh264enc`) on Windows; V4L2 encoders
//! (`v4l2h264enc`) on ARM boards.
//!
//! GStreamer is LGPL and linked dynamically from the system; the Rust bindings are safe. One
//! pipeline per layer: `appsrc` (NV12 frames) → hardware encoder → Constrained Baseline caps →
//! `h264parse` (SPS/PPS before every keyframe, and Annex B from encoders that only give AVC, like
//! VideoToolbox) → `appsink` (Annex B access units). Encoders that can't be built (no plugin, no
//! GPU, unsupported size) return `None`, and [`FallbackEncoder`] switches to OpenH264 if one fails
//! or stops giving pictures later.
//!
//! [`FallbackEncoder`]: crate::video_codec::FallbackEncoder

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};
use teamcraft_proto::{VideoCodec, VideoLayerInfo};
use tracing::{debug, info, warn};

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
  /// Row length and luma plane height of the NV12 buffers the caps describe: GStreamer pads rows
  /// to four bytes, so a 682 pixel wide screen layer has 684 byte rows.
  stride: usize,
  slice_height: usize,
  element: &'static str,
  has_delivered: bool,
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
         video/x-h264,profile=constrained-baseline ! h264parse config-interval=-1 ! \
         video/x-h264,stream-format=byte-stream,alignment=au ! appsink name=sink sync=false",
        element.name,
        (element.properties)(bitrate_kbps),
      );
      match Self::build(&description, element.name, layer, content) {
        Ok(encoder) => return Some(encoder),
        Err(error) => info!(%error, element = element.name, "hardware encoder unusable"),
      }
    }
    info!("no usable hardware H.264 encoder; encoding in software");
    None
  }

  fn build(
    description: &str,
    element: &'static str,
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
    let info = gstreamer_video::VideoInfo::builder(
      gstreamer_video::VideoFormat::Nv12,
      u32::from(layer.width),
      u32::from(layer.height),
    )
    .fps(gst::Fraction::new(framerate, 1))
    .build()
    .map_err(gstreamer_error)?;
    let (stride, slice_height) = nv12_layout(&info)?;
    let caps = info.to_caps().map_err(gstreamer_error)?;
    source.set_caps(Some(&caps));
    debug!(
      element,
      ?content,
      width = layer.width,
      height = layer.height,
      stride,
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
      stride,
      slice_height,
      element,
      has_delivered: false,
    })
  }

  /// The pipeline's first error message (with the element's debug detail), if it reported one.
  fn bus_error(&self) -> Option<String> {
    let bus = self.pipeline.bus()?;
    let message = bus.pop_filtered(&[gst::MessageType::Error])?;
    let gst::MessageView::Error(error) = message.view() else {
      return None;
    };
    let text = match error.debug() {
      Some(debug) => format!("{} ({debug})", error.error()),
      None => error.error().to_string(),
    };
    warn!(element = self.element, error = %text, "hardware encoder pipeline error");
    Some(text)
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
    let mut buffer = gst::Buffer::from_mut_slice(nv12_bytes(&frame, self.stride, self.slice_height));
    buffer
      .make_mut()
      .set_pts(gst::ClockTime::from_useconds(frame.timestamp_us()));
    if let Err(flow) = self.source.push_buffer(buffer) {
      let reason = self.bus_error().unwrap_or_else(|| format!("{flow:?}"));
      return Err(VideoCodecError::Codec(format!("GStreamer: {reason}")));
    }
    let Some(sample) = self
      .sink
      .try_pull_sample(gst::ClockTime::from_mseconds(PULL_TIMEOUT_MS))
    else {
      if let Some(error) = self.bus_error() {
        return Err(VideoCodecError::Codec(format!("GStreamer: {error}")));
      }
      return Ok(None);
    };
    if !self.has_delivered {
      self.has_delivered = true;
      debug!(
        element = self.element,
        width = self.resolution.width,
        height = self.resolution.height,
        "the hardware encoder delivers pictures"
      );
    }
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
  /// Stops the pipeline on a thread of its own: an encoder dropped because it stalled may have
  /// its streaming thread stuck in the driver, and stopping waits for that thread. The encoder
  /// thread (and with it the call) must not wait as well.
  fn drop(&mut self) {
    let pipeline = self.pipeline.clone();
    let element = self.element;
    let stop = move || {
      // Tearing down: a failing state change leaves nothing else to do.
      let _ = pipeline.set_state(gst::State::Null);
      debug!(element, "hardware encoder stopped");
    };
    let spawned = std::thread::Builder::new()
      .name("gst-encoder-stop".into())
      .spawn(stop.clone());
    if let Err(error) = spawned {
      warn!(%error, "no thread to stop the hardware encoder; stopping it here");
      stop();
    }
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

// Media Foundation's H.264 encoder MFT (NVIDIA, AMD and Intel hardware, else Microsoft's software
// MFT); needs GStreamer's MSVC runtime. Compiles, untested (no Windows machine in development).
#[cfg(target_os = "windows")]
const HARDWARE_ELEMENTS: &[HardwareElement] = &[HardwareElement {
  name: "mfh264enc",
  properties: |bitrate_kbps| {
    format!(
      "bitrate={bitrate_kbps} rc-mode=cbr low-latency=true bframes=0 gop-size={KEYFRAME_INTERVAL_FRAMES}"
    )
  },
}];

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const HARDWARE_ELEMENTS: &[HardwareElement] = &[];

/// Row length and luma plane height (in rows) of NV12 buffers described by `info`, as
/// [`nv12_bytes`] lays them out: the chroma plane follows the luma plane with the same stride.
fn nv12_layout(info: &gstreamer_video::VideoInfo) -> Result<(usize, usize), VideoCodecError> {
  let (Some(&luma_stride), Some(&chroma_stride), Some(&chroma_offset)) =
    (info.stride().first(), info.stride().get(1), info.offset().get(1))
  else {
    return Err(VideoCodecError::Codec("NV12 without two planes".into()));
  };
  let stride = usize::try_from(luma_stride).map_err(|_| VideoCodecError::Codec("negative stride".into()))?;
  if stride == 0 || i64::from(luma_stride) != i64::from(chroma_stride) || chroma_offset % stride != 0 {
    return Err(VideoCodecError::Codec(format!(
      "unexpected NV12 layout: strides {:?}, offsets {:?}",
      info.stride(),
      info.offset()
    )));
  }
  Ok((stride, chroma_offset / stride))
}

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

  /// A screen layer whose width isn't a multiple of four: GStreamer pads its rows, and the
  /// buffers have to match (they were too short before, issue #683). Hardware only, like above.
  #[test]
  fn a_layer_with_padded_rows_encodes() {
    let screen = VideoResolution {
      width: 1364,
      height: 768,
    };
    let layer = simulcast_layers(screen, 30, 2_000_000, 2)[0];
    assert_eq!(layer.width % 4, 2, "the half-size layer needs padded rows");
    let Some(mut encoder) = GstreamerEncoder::new(&layer, VideoContent::Screen) else {
      assert!(
        std::env::var_os("TEAMCRAFT_REQUIRE_HARDWARE_ENCODER").is_none(),
        "no hardware encoder"
      );
      return;
    };
    assert_eq!(encoder.stride, usize::from(layer.width) + 2);
    let mut pictures = 0;
    for index in 0..20u64 {
      let frame = test_pattern(screen, index, index * 33_333).unwrap();
      if encoder.encode(&frame, index == 0).unwrap().is_some() {
        pictures += 1;
      }
    }
    assert!(pictures >= 15, "only {pictures} of 20 frames came out");
  }

  #[test]
  fn nv12_buffers_follow_gstreamers_row_padding() {
    gst::init().unwrap();
    let layout = |width, height| {
      let info = gstreamer_video::VideoInfo::builder(gstreamer_video::VideoFormat::Nv12, width, height)
        .build()
        .unwrap();
      nv12_layout(&info).unwrap()
    };
    assert_eq!(layout(1280, 720), (1280, 720));
    assert_eq!(layout(682, 384), (684, 384));
  }
}
