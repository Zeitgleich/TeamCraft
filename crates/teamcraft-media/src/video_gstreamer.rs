//! Hardware H.264 encoding through GStreamer: VA-API (`vah264enc`) on Linux, VideoToolbox
//! (`vtenc_h264`) on macOS, Media Foundation (`mfh264enc`) on Windows; V4L2 encoders
//! (`v4l2h264enc`) on ARM boards.
//!
//! GStreamer is LGPL and linked dynamically from the system; the Rust bindings are safe. One
//! pipeline per layer: `appsrc` (NV12 frames) → hardware encoder → Baseline caps (in the profile
//! name the element accepts, [`HardwareElement::profile`]) → `h264parse` (SPS/PPS before every
//! keyframe, and Annex B from encoders that only give AVC, like VideoToolbox) → `appsink` (Annex B
//! access units). Each picture is the one of the frame just pushed, told by its timestamp: one
//! that comes too late is dropped rather than handed out with a later frame (#990). Encoders that
//! can't be built (no plugin, no GPU, unsupported size) return `None`, and [`FallbackEncoder`]
//! switches to OpenH264 if one fails or stops giving pictures later.
//!
//! [`FallbackEncoder`]: crate::video_codec::FallbackEncoder

use std::collections::VecDeque;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};
use teamcraft_proto::{VideoCodec, VideoLayerInfo};
use tracing::{debug, info, warn};

use crate::video::{VideoFrame, VideoResolution};
use crate::video_codec::{EncodedPicture, VideoCodecError, VideoContent, VideoEncoder, nv12_bytes};

/// Keyframe distance in frames, like OpenH264's.
const KEYFRAME_INTERVAL_FRAMES: u32 = 300;
/// How long one frame may take through the encoder before it counts as dropped: its picture, if
/// it comes later, is dropped too (#990).
const PULL_TIMEOUT_MS: u64 = 50;
/// How long a pipeline's first picture may take: the first VideoToolbox session of a process
/// gives it after 55 to 115 ms (a 1440p or 4K screen layer, a 720p camera; #990), later sessions
/// within 35 ms.
const FIRST_PULL_TIMEOUT_MS: u64 = 500;

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
  /// Whether this pipeline gave a picture yet: until then it may take [`FIRST_PULL_TIMEOUT_MS`].
  has_answered: bool,
  /// The next frame becomes a keyframe: a picture was dropped that later ones would refer to.
  wants_keyframe: bool,
  /// The timestamp of the frame [`VideoEncoder::start`] pushed, until its picture is taken.
  maybe_started_us: Option<u64>,
  /// What holds a screen layer at its bitrate, for elements that need it
  /// ([`HardwareElement::is_screen_quality_governed`]).
  maybe_governed: Option<Governed>,
}

/// A screen layer whose quality a [`QualityGovernor`] sets: the encoder is rebuilt at each new
/// quality (VideoToolbox keeps the quality its session started with).
struct Governed {
  hardware: &'static HardwareElement,
  layer: VideoLayerInfo,
  governor: QualityGovernor,
}

/// The platform's hardware encoder element and its settings.
struct HardwareElement {
  name: &'static str,
  /// The H.264 profile asked for downstream of the encoder, as the element names it: VA-API takes
  /// `constrained-baseline`, VideoToolbox (`vtenc_h264`) only `baseline` and fails to negotiate
  /// with anything else, so no picture ever came out of it (issue #683). Both encode without
  /// B-frames, which is what every receiver decodes.
  profile: &'static str,
  /// `gst-launch` style properties for the layer's bitrate (kbit/s) and content.
  properties: fn(bitrate_kbps: u32, content: VideoContent) -> String,
  /// Whether screen layers get a [`QualityGovernor`] for the element's `quality`: its own rate
  /// control lets them send far more than their bitrate.
  is_screen_quality_governed: bool,
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
    for element in HARDWARE_ELEMENTS {
      if gst::ElementFactory::find(element.name).is_none() {
        debug!(element = element.name, "hardware encoder element not installed");
        continue;
      }
      let is_governed = element.is_screen_quality_governed && content == VideoContent::Screen;
      let maybe_quality = is_governed.then_some(SCREEN_QUALITY);
      let description = pipeline_description(element, layer, content, maybe_quality);
      match Self::build(&description, element.name, layer, content) {
        Ok(mut encoder) => {
          encoder.maybe_governed = is_governed.then(|| Governed {
            hardware: element,
            layer: *layer,
            governor: QualityGovernor::new(layer.max_bitrate_bps),
          });
          return Some(encoder);
        },
        Err(error) => info!(%error, element = element.name, "hardware encoder unusable"),
      }
    }
    info!("no usable hardware H.264 encoder; encoding in software");
    None
  }

  /// Notes the picture of the frame of `timestamp_us` for the [`QualityGovernor`], if the layer
  /// has one, and rebuilds the encoder at the quality it asks for: the next picture is a keyframe.
  /// Where that fails the layer keeps its encoder and quality, ungoverned.
  fn govern(&mut self, timestamp_us: u64, bytes: usize, is_keyframe: bool) {
    let Some(governed) = &mut self.maybe_governed else {
      return;
    };
    let Some(quality) = governed.governor.note_picture(timestamp_us, bytes, is_keyframe) else {
      return;
    };
    self.rebuild_at(quality);
  }

  /// Rebuilds a governed screen layer's encoder at `quality`.
  fn rebuild_at(&mut self, quality: f64) {
    let Some(governed) = &self.maybe_governed else {
      return;
    };
    let layer = governed.layer;
    let description = pipeline_description(governed.hardware, &layer, VideoContent::Screen, Some(quality));
    match Self::build(&description, self.element, &layer, VideoContent::Screen) {
      Ok(rebuilt) => {
        debug!(
          element = self.element,
          width = layer.width,
          quality,
          "screen layer at a new quality for its bitrate"
        );
        // The old pipeline stops on a thread of its own (`Drop`).
        let mut old = std::mem::replace(self, rebuilt);
        self.has_delivered = true;
        self.maybe_governed = old.maybe_governed.take();
        if let Some(governed) = &mut self.maybe_governed {
          governed.governor.set_quality(quality);
        }
      },
      Err(error) => {
        warn!(%error, element = self.element, "screen layer keeps its quality");
        self.maybe_governed = None;
      },
    }
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
      has_answered: false,
      wants_keyframe: false,
      maybe_started_us: None,
      maybe_governed: None,
    })
  }

  /// Drops the pictures waiting in the sink: they come from frames whose wait ran out, and sent
  /// now each would show a frame late, and every picture after it as well (#990). Whether there
  /// were any: the next picture must then be a keyframe, as the ones after them refer to them.
  fn drop_late_pictures(&self) -> bool {
    let mut has_dropped = false;
    while let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::ZERO) {
      debug!(
        element = self.element,
        timestamp_us = ?sample_timestamp_us(&sample),
        "dropping a picture that came too late"
      );
      has_dropped = true;
    }
    has_dropped
  }

  /// The picture of the frame of `timestamp_us`, just pushed, with any late ones before it
  /// (dropped), or `None` when it doesn't come within its wait. Where a late picture came, a
  /// picture that isn't a keyframe refers to it and is dropped as well, and the next frame becomes
  /// a keyframe.
  fn pull_picture(&mut self, timestamp_us: u64) -> Option<gst::Sample> {
    let timeout_ms = if self.has_answered {
      PULL_TIMEOUT_MS
    } else {
      FIRST_PULL_TIMEOUT_MS
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut has_late = false;
    loop {
      let left = deadline.saturating_duration_since(std::time::Instant::now());
      let left = gst::ClockTime::from_nseconds(u64::try_from(left.as_nanos()).unwrap_or(u64::MAX));
      let sample = self.sink.try_pull_sample(left)?;
      // A picture without a timestamp counts as the frame's, as every picture did before #990.
      let is_late = sample_timestamp_us(&sample).is_some_and(|picture_us| picture_us < timestamp_us);
      if is_late {
        debug!(element = self.element, "dropping a picture that came too late");
        has_late = true;
        continue;
      }
      self.has_answered = true;
      let is_keyframe = sample
        .buffer()
        .is_some_and(|buffer| !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT));
      if has_late && !is_keyframe {
        self.wants_keyframe = true;
        return None;
      }
      return Some(sample);
    }
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

  fn name(&self) -> &'static str {
    self.element
  }

  fn encode(
    &mut self,
    frame: &VideoFrame,
    is_keyframe_forced: bool,
  ) -> Result<Option<EncodedPicture>, VideoCodecError> {
    self.start(frame, is_keyframe_forced)?;
    self.finish(frame, is_keyframe_forced)
  }

  /// Pushes `frame` into the pipeline: the encoder works on it while the other layers get theirs.
  fn start(&mut self, frame: &VideoFrame, is_keyframe_forced: bool) -> Result<(), VideoCodecError> {
    let frame = frame.at_size(self.resolution)?;
    let has_dropped = self.drop_late_pictures();
    let is_keyframe_forced = is_keyframe_forced || has_dropped || std::mem::take(&mut self.wants_keyframe);
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
    self.maybe_started_us = Some(frame.timestamp_us());
    Ok(())
  }

  /// Waits for the picture of the frame [`Self::start`] pushed (pushes `frame` first if it
  /// didn't).
  fn finish(
    &mut self,
    frame: &VideoFrame,
    is_keyframe_forced: bool,
  ) -> Result<Option<EncodedPicture>, VideoCodecError> {
    if self.maybe_started_us.is_none() {
      self.start(frame, is_keyframe_forced)?;
    }
    let timestamp_us = self.maybe_started_us.take().unwrap_or(frame.timestamp_us());
    let Some(sample) = self.pull_picture(timestamp_us) else {
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
    let data = buffer
      .map_readable()
      .map_err(gstreamer_error)?
      .as_slice()
      .to_vec();
    self.govern(timestamp_us, data.len(), is_keyframe);
    Ok(Some(EncodedPicture { is_keyframe, data }))
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
    profile: "constrained-baseline",
    properties: |bitrate_kbps, _content| {
      format!(
        "bitrate={bitrate_kbps} rate-control=cbr key-int-max={KEYFRAME_INTERVAL_FRAMES} b-frames=0 \
         ref-frames=1 cabac=false"
      )
    },
    is_screen_quality_governed: false,
  },
  HardwareElement {
    name: "v4l2h264enc",
    profile: "constrained-baseline",
    properties: |bitrate_kbps, _content| {
      let bitrate_bps = u64::from(bitrate_kbps) * 1000;
      format!(
        "extra-controls=\"controls,video_bitrate={bitrate_bps},h264_i_frame_period={KEYFRAME_INTERVAL_FRAMES}\""
      )
    },
    is_screen_quality_governed: false,
  },
];

/// macOS: VideoToolbox. Tested on Apple silicon with GStreamer 1.28 (the release bundles the same
/// version).
///
/// Cameras at a constant bitrate (#967): with only an average bitrate (`AverageBitRate`) the
/// encoder stops spending bits on a calm picture long before it looks good. A 720p camera layer
/// meant for 1.5 Mbit/s sent 155 to 306 kbit/s in calls, and a calm test picture 600 kbit/s at
/// 41 dB PSNR; at a constant bitrate it sends 1.45 Mbit/s at 46 dB, and the 360p and 180p layers,
/// which sent a tenth of theirs, their whole bitrate too (`examples/camera_bitrate.rs`). A
/// quality floor (`quality`) instead made VideoToolbox ignore the bitrate (9 Mbit/s at 0.9).
/// VideoToolbox has a constant bitrate on Apple silicon from macOS 13; elsewhere GStreamer caps
/// the average with data rate limits instead.
///
/// Screen shares keep the average bitrate: a still screen needs next to nothing between its
/// changes, where a constant bitrate spends the whole bitrate on every change (5.8 of 6.6 Mbit/s
/// for a line being typed on a 1440p screen, against 22 kbit/s). But at an average bitrate
/// VideoToolbox encodes at the `quality` GStreamer always sets (0.5 by default) and ignores the
/// bitrate and the data rate limits (#979): scrolling text sent 9 Mbit/s in a 720p layer meant for
/// 1.7, and as much with the bitrate set to 100 kbit/s. So a [`QualityGovernor`] picks a screen
/// layer's quality for its bitrate.
#[cfg(target_os = "macos")]
const HARDWARE_ELEMENTS: &[HardwareElement] = &[HardwareElement {
  name: "vtenc_h264",
  profile: "baseline",
  properties: |bitrate_kbps, content| {
    let rate_control = match content {
      VideoContent::Camera => "cbr",
      VideoContent::Screen => "abr",
    };
    format!(
      "bitrate={bitrate_kbps} rate-control={rate_control} realtime=true \
       allow-frame-reordering=false max-keyframe-interval={KEYFRAME_INTERVAL_FRAMES}"
    )
  },
  is_screen_quality_governed: true,
}];

// Media Foundation's H.264 encoder MFT (NVIDIA, AMD and Intel hardware, else Microsoft's software
// MFT); needs GStreamer's MSVC runtime. Compiles, untested (no Windows machine in development).
#[cfg(target_os = "windows")]
const HARDWARE_ELEMENTS: &[HardwareElement] = &[HardwareElement {
  name: "mfh264enc",
  profile: "constrained-baseline",
  properties: |bitrate_kbps, _content| {
    format!(
      "bitrate={bitrate_kbps} rc-mode=cbr low-latency=true bframes=0 gop-size={KEYFRAME_INTERVAL_FRAMES}"
    )
  },
  is_screen_quality_governed: false,
}];

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const HARDWARE_ELEMENTS: &[HardwareElement] = &[];

/// The `gst-launch` description of a pipeline encoding `layer` with `element`, at `maybe_quality`
/// (the element's `quality`) where given.
fn pipeline_description(
  element: &HardwareElement,
  layer: &VideoLayerInfo,
  content: VideoContent,
  maybe_quality: Option<f64>,
) -> String {
  let bitrate_kbps = (layer.max_bitrate_bps / 1000).max(1);
  // GStreamer reads the number the same in every locale.
  let quality = maybe_quality.map_or(String::new(), |quality| format!(" quality={quality:.3}"));
  format!(
    "appsrc name=source is-live=true format=time ! {} {}{quality} ! \
     video/x-h264,profile={} ! h264parse config-interval=-1 ! \
     video/x-h264,stream-format=byte-stream,alignment=au ! appsink name=sink sync=false",
    element.name,
    (element.properties)(bitrate_kbps, content),
    element.profile,
  )
}

/// A screen layer's highest quality: GStreamer's default, what screen shares had before #979
/// (the text of a still 1440p screen at about 36 dB PSNR for a few dozen kbit/s).
const SCREEN_QUALITY: f64 = 0.5;
/// How many of the last pictures a layer's bitrate is measured over: scrolled text gives some
/// pictures several times the size of the others.
const MEASURED_PICTURES: usize = 8;
/// Pictures measured at a quality before it changes again, unless they span a second: a scroll
/// that starts is noticed within about a tenth of a second, a screen that stands still by its
/// next picture.
const MIN_MEASURED_PICTURES: usize = 4;
/// How long a quality holds before it may rise again: each change costs a keyframe, and a layer
/// that just went down shouldn't come straight back up while the screen still scrolls.
const MIN_RISE_INTERVAL_US: u64 = 1_000_000;
/// A picture counts over the time since the one before, within these bounds. A still screen's
/// picture comes once a second (`video_pipeline::SCREEN_IDLE_REFRESH`): after that long the
/// pictures before don't tell what comes, and the measuring starts afresh.
const MIN_PICTURE_INTERVAL_US: u64 = 1_000;
const STILL_INTERVAL_US: u64 = 1_000_000;
/// How much the bitrate grows per unit of quality (natural log): about 4 above a quality of 0.25
/// and 12 below it, with scrolling text (`examples/screen_bench.rs`). A layer over its bitrate
/// steps down by the flatter slope (a step into the steeper part goes too far, and the step up
/// catches it); a layer below steps up by the steeper one, so it doesn't overshoot again.
const FALL_SLOPE: f64 = 4.0;
const RISE_SLOPE: f64 = 12.0;
/// A layer below this share of its bitrate gets a higher quality, aimed at [`AIM_SHARE`]. Between
/// it and the whole bitrate the quality stays.
const LOW_SHARE: f64 = 0.5;
const AIM_SHARE: f64 = 0.6;
/// A layer below this share of its bitrate goes straight back to [`SCREEN_QUALITY`]: its screen
/// stands still (scrolled text at the lowest quality still needs more than a tenth).
const CALM_SHARE: f64 = 0.05;
/// Smaller changes than this aren't made.
const MIN_QUALITY_STEP: f64 = 0.02;
/// How long after a keyframe the pictures aren't measured: VideoToolbox makes the next few
/// several times the size they settle at (scrolled text at 0.11: 132, 41, 30 and 86 kbit, then
/// about 10), and taken for the content they sent the quality down again after every change.
const KEYFRAME_SETTLE_US: u64 = 200_000;

/// Holds a screen layer at most at its bitrate where the encoder doesn't (#979): VideoToolbox at
/// an average bitrate encodes at its quality whatever that costs. From the pictures' sizes over
/// the last few frames it lowers the quality while the layer sends more than its bitrate (in steps
/// sized by how far over it is), and raises it again, up to [`SCREEN_QUALITY`], once the layer
/// sends less than half of it: a still or typed-on screen keeps its quality, scrolling text in a
/// lower layer is held at its bitrate within a few frames, and its text sharpens again a second
/// after it stops. Keyframes aren't measured, nor the pictures of [`KEYFRAME_SETTLE_US`] after
/// one: they come on request or with a new quality, not from the content.
#[derive(Debug)]
struct QualityGovernor {
  target_bps: f64,
  quality: f64,
  maybe_last_us: Option<u64>,
  maybe_changed_us: Option<u64>,
  /// Bits and time (µs) of the last pictures at the current quality.
  measured: VecDeque<(f64, u64)>,
  maybe_keyframe_us: Option<u64>,
}

impl QualityGovernor {
  fn new(target_bps: u32) -> Self {
    Self {
      target_bps: f64::from(target_bps.max(1)),
      quality: SCREEN_QUALITY,
      maybe_last_us: None,
      maybe_changed_us: None,
      measured: VecDeque::with_capacity(MEASURED_PICTURES),
      maybe_keyframe_us: None,
    }
  }

  /// Notes a picture of `bytes` from the frame of `timestamp_us`; the quality the frames from now
  /// on need, when it is another ([`Self::set_quality`] once the encoder has it).
  fn note_picture(&mut self, timestamp_us: u64, bytes: usize, is_keyframe: bool) -> Option<f64> {
    let maybe_interval_us = self.maybe_last_us.map(|last_us| {
      timestamp_us
        .saturating_sub(last_us)
        .clamp(MIN_PICTURE_INTERVAL_US, STILL_INTERVAL_US)
    });
    self.maybe_last_us = Some(timestamp_us);
    if is_keyframe {
      self.maybe_keyframe_us = Some(timestamp_us);
    }
    let interval_us = maybe_interval_us?;
    let is_settling = self
      .maybe_keyframe_us
      .is_some_and(|keyframe_us| timestamp_us.saturating_sub(keyframe_us) < KEYFRAME_SETTLE_US);
    if is_settling {
      return None;
    }
    if interval_us == STILL_INTERVAL_US {
      self.measured.clear();
    } else if self.measured.len() == MEASURED_PICTURES {
      self.measured.pop_front();
    }
    self.measured.push_back((bytes as f64 * 8.0, interval_us));
    let measured_us: u64 = self.measured.iter().map(|(_, us)| us).sum();
    if self.measured.len() < MIN_MEASURED_PICTURES && measured_us < STILL_INTERVAL_US {
      return None;
    }
    let bits: f64 = self.measured.iter().map(|(bits, _)| bits).sum();
    let share = bits / (measured_us as f64 / 1e6) / self.target_bps;
    let may_rise = self
      .maybe_changed_us
      .is_none_or(|changed_us| timestamp_us.saturating_sub(changed_us) >= MIN_RISE_INTERVAL_US);
    let quality = if share > 1.0 {
      self.quality - share.ln() / FALL_SLOPE
    } else if share < CALM_SHARE && may_rise {
      SCREEN_QUALITY
    } else if share < LOW_SHARE && may_rise {
      self.quality + (AIM_SHARE / share).ln() / RISE_SLOPE
    } else {
      self.quality
    }
    .clamp(0.0, SCREEN_QUALITY);
    let is_change = (quality - self.quality).abs() >= MIN_QUALITY_STEP
      || (quality == SCREEN_QUALITY && quality > self.quality);
    is_change.then_some(quality)
  }

  /// The encoder encodes at `quality` from now on: it is measured afresh.
  fn set_quality(&mut self, quality: f64) {
    self.quality = quality;
    self.maybe_changed_us = self.maybe_last_us;
    self.measured.clear();
  }
}

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

/// The timestamp (µs) of the frame `sample` is the picture of, as pushed into the pipeline: the
/// encoder moves its pictures 1000 hours on (`GstVideoEncoder`, for decoding times before 0) and
/// says so in the segment.
fn sample_timestamp_us(sample: &gst::Sample) -> Option<u64> {
  let pts = sample.buffer()?.pts()?;
  let segment = sample.segment()?.downcast_ref::<gst::ClockTime>()?;
  Some(segment.to_running_time(pts)?.useconds())
}

fn gstreamer_error(error: impl std::fmt::Debug) -> VideoCodecError {
  VideoCodecError::Codec(format!("GStreamer: {error:?}"))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::picture_check::{PictureContent, picture_content};
  use crate::video::{screen_layers, simulcast_layers};
  use crate::video_capture::test_pattern;
  use crate::video_codec::{OpenH264Decoder, VideoDecoder};
  use teamcraft_proto::{ScreenContentMode, ScreenShareResolution};

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
    let mut sources = Vec::new();
    for index in 0..40u64 {
      let frame = test_pattern(HD, index, index * 33_333).unwrap();
      if let Some(picture) = encoder.encode(&frame, index == 20).unwrap() {
        pictures.push((index, picture));
      }
      sources.push(frame);
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
        // The picture itself, not just a stream that decodes (issue #683: video arrived black):
        // an encoder may give a frame's picture a frame or two later.
        assert_eq!(picture_content(&frame), PictureContent::Detailed, "frame {index}");
        let first_source = usize::try_from(index.saturating_sub(2)).unwrap();
        let psnr = sources[first_source..=usize::try_from(*index).unwrap()]
          .iter()
          .map(|source| luma_psnr(source, &frame))
          .fold(0.0, f64::max);
        assert!(psnr > 28.0, "frame {index}: PSNR {psnr:.1} dB");
        decoded += 1;
      }
    }
    assert!(
      decoded + 2 >= pictures.len(),
      "decoded {decoded} of {}",
      pictures.len()
    );
  }

  /// The frame counter of a [`test_pattern`] picture (8 bits in blocks along its top rows).
  fn frame_counter(frame: &VideoFrame) -> u64 {
    let size = frame.resolution();
    let (width, height) = (usize::from(size.width), usize::from(size.height));
    let row = height / 32;
    (0..8)
      .filter(|bit| frame.planes().0[row * width + (2 * bit + 1) * width / 16] > 128)
      .map(|bit| 1 << bit)
      .sum()
  }

  /// #990: every picture shows the frame it was encoded for, in a camera layer, a screen layer
  /// at 30 fps and once a second (a still screen sends its changes only), and in a screen layer
  /// its [`QualityGovernor`] rebuilt. A picture that missed its wait came out with the next frame
  /// instead, and every one after it too: a still screen's change up to a second late. Only the
  /// process's first VideoToolbox session was that slow: run alone, the camera here is it.
  /// Hardware only, like above.
  #[test]
  #[cfg_attr(
    not(feature = "openh264-source"),
    ignore = "decodes with OpenH264: --features openh264-source"
  )]
  fn every_hardware_picture_shows_its_own_frame() {
    let screen = VideoResolution {
      width: 2560,
      height: 1440,
    };
    let screen_layer = *screen_layers(
      screen,
      30,
      ScreenContentMode::Detail,
      &ScreenShareResolution::Standard,
    )
    .last()
    .unwrap();
    let camera_layer = simulcast_layers(HD, 30, 1_500_000, 1)[0];
    let cases = [
      (camera_layer, VideoContent::Camera, 33_333u64),
      (screen_layer, VideoContent::Screen, 33_333),
      (screen_layer, VideoContent::Screen, 1_000_000),
    ];
    for (layer, content, interval_us) in cases {
      let Some(mut encoder) = GstreamerEncoder::new(&layer, content) else {
        assert!(
          std::env::var_os("TEAMCRAFT_REQUIRE_HARDWARE_ENCODER").is_none(),
          "no hardware encoder"
        );
        return;
      };
      let size = VideoResolution {
        width: layer.width,
        height: layer.height,
      };
      let mut decoder = OpenH264Decoder::new().unwrap();
      let mut shown = Vec::new();
      for index in 0..24u64 {
        if index == 12 && encoder.maybe_governed.is_some() {
          // A new quality, as for scrolled text: the encoder is rebuilt.
          encoder.rebuild_at(SCREEN_QUALITY / 2.0);
          assert!(!encoder.has_answered, "rebuilt");
        }
        let frame = test_pattern(size, index, index * interval_us).unwrap();
        if let Some(picture) = encoder.encode(&frame, index == 0).unwrap()
          && let Some(decoded) = decoder.decode(&picture.data, frame.timestamp_us()).unwrap()
        {
          shown.push((index, frame_counter(&decoded)));
        }
      }
      let late: Vec<_> = shown.iter().filter(|(index, counter)| index != counter).collect();
      assert!(
        late.is_empty(),
        "{content:?} {}x{} every {interval_us} µs: (frame, shown) {late:?}",
        layer.width,
        layer.height
      );
      assert!(shown.len() >= 20, "only {} of 24 pictures", shown.len());
    }
  }

  /// An encoder pipeline that holds each frame for `sleep-time` µs (`identity`) and passes it on
  /// as its picture: a slow encoder, in every GStreamer.
  fn slow_pipeline(sleep_us: u32) -> GstreamerEncoder {
    gst::init().unwrap();
    let layer = simulcast_layers(HD, 30, 1_500_000, 1)[0];
    let description = format!(
      "appsrc name=source is-live=true format=time ! identity name=slow sleep-time={sleep_us} ! \
       appsink name=sink sync=false"
    );
    GstreamerEncoder::build(&description, "identity", &layer, VideoContent::Camera).unwrap()
  }

  fn set_sleep(encoder: &GstreamerEncoder, sleep_us: u32) {
    let slow = encoder.pipeline.by_name("slow").unwrap();
    slow.set_property("sleep-time", sleep_us);
  }

  /// The frame counter of a picture `slow_pipeline` passed on (the frame's NV12 bytes).
  fn passed_counter(picture: &EncodedPicture) -> u64 {
    let frame = VideoFrame::new(HD, 0, picture.data.clone()).unwrap();
    frame_counter(&frame)
  }

  /// #990: a pipeline's first picture may take longer than the others (VideoToolbox starts its
  /// first session in a process in up to 110 ms): it is waited for, not handed out with the next
  /// frame, after which every picture came a frame late.
  #[test]
  fn a_slow_first_picture_is_waited_for() {
    let mut encoder = slow_pipeline(150_000);
    let first = test_pattern(HD, 0, 0).unwrap();
    let picture = encoder.encode(&first, true).unwrap().expect("the first picture");
    assert_eq!(passed_counter(&picture), 0);
    set_sleep(&encoder, 0);
    for index in 1..6u64 {
      let frame = test_pattern(HD, index, index * 33_333).unwrap();
      let picture = encoder.encode(&frame, false).unwrap().expect("a picture");
      assert_eq!(passed_counter(&picture), index, "the picture of frame {index}");
    }
  }

  /// #990: a picture that misses its wait is dropped when it comes, not handed out with the next
  /// frame (which would leave every picture after it a frame late), and that frame is a keyframe:
  /// the pictures after the dropped one would refer to it.
  #[test]
  fn a_late_picture_is_dropped_and_the_next_frame_is_a_keyframe() {
    let mut encoder = slow_pipeline(0);
    let keyframe_requests = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let requests = keyframe_requests.clone();
    let sink_pad = encoder.sink.static_pad("sink").unwrap();
    sink_pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
      if let Some(gst::PadProbeData::Event(event)) = &info.data
        && gstreamer_video::DownstreamForceKeyUnitEvent::parse(event).is_ok()
      {
        requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      }
      gst::PadProbeReturn::Ok
    });
    let frame = |index: u64| test_pattern(HD, index, index * 33_333).unwrap();
    let picture = encoder.encode(&frame(0), false).unwrap().expect("a picture");
    assert_eq!(passed_counter(&picture), 0);
    // Frame 1 takes three times its wait.
    set_sleep(&encoder, 150_000);
    assert!(
      encoder.encode(&frame(1), false).unwrap().is_none(),
      "frame 1 is late"
    );
    set_sleep(&encoder, 0);
    std::thread::sleep(std::time::Duration::from_millis(200));
    for index in 2..6u64 {
      let picture = encoder.encode(&frame(index), false).unwrap().expect("a picture");
      assert_eq!(passed_counter(&picture), index, "the picture of frame {index}");
    }
    assert_eq!(
      keyframe_requests.load(std::sync::atomic::Ordering::SeqCst),
      1,
      "frame 2 is a keyframe"
    );
  }

  #[test]
  fn a_picture_names_its_frame_through_the_encoders_segment() {
    gst::init().unwrap();
    // As VideoToolbox gives them: 1000 hours on, said so by the segment's start.
    let offset = gst::ClockTime::from_seconds(1000 * 3600);
    let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
    segment.set_start(offset);
    segment.set_position(offset);
    let mut buffer = gst::Buffer::new();
    buffer
      .get_mut()
      .unwrap()
      .set_pts(offset + gst::ClockTime::from_useconds(1_033_333));
    let sample = gst::Sample::builder().buffer(&buffer).segment(&segment).build();
    assert_eq!(sample_timestamp_us(&sample), Some(1_033_333));
    let unstamped = gst::Sample::builder().buffer(&gst::Buffer::new()).build();
    assert_eq!(sample_timestamp_us(&unstamped), None);
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

  /// #967: a calm camera picture uses its layer's bitrate. VideoToolbox at an average bitrate
  /// sent a tenth of it, and the picture looked soft. Hardware only, like above.
  #[test]
  fn a_calm_camera_picture_uses_its_bitrate() {
    const FRAMES: u64 = 120;
    const SETTLED: u64 = 30;
    let layer = simulcast_layers(HD, 30, 1_500_000, 1)[0];
    let Some(mut encoder) = GstreamerEncoder::new(&layer, VideoContent::Camera) else {
      assert!(
        std::env::var_os("TEAMCRAFT_REQUIRE_HARDWARE_ENCODER").is_none(),
        "no hardware encoder"
      );
      return;
    };
    let still = test_pattern(HD, 0, 0).unwrap();
    let mut settled_bytes = 0;
    for index in 0..FRAMES {
      // The still picture with a camera's faint noise, at 30 fps.
      let mut data = still.data().to_vec();
      for (offset, value) in data.iter_mut().enumerate().step_by(7) {
        let noise = (offset as u64 ^ index.wrapping_mul(2_654_435_761)) % 3;
        *value = value.saturating_add(noise as u8);
      }
      let frame = VideoFrame::new(HD, index * 33_333, data).unwrap();
      if let Some(picture) = encoder.encode(&frame, index == 0).unwrap()
        && index >= SETTLED
      {
        settled_bytes += picture.data.len() as u64;
      }
    }
    let seconds = (FRAMES - SETTLED) as f64 / 30.0;
    let bitrate_bps = settled_bytes as f64 * 8.0 / seconds;
    let share = bitrate_bps / f64::from(layer.max_bitrate_bps);
    assert!(
      share > 0.7,
      "{} sent {:.0} kbit/s of {} kbit/s",
      encoder.element,
      bitrate_bps / 1000.0,
      layer.max_bitrate_bps / 1000
    );
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn videotoolbox_sends_cameras_at_a_constant_bitrate_and_screens_on_average() {
    let element = &HARDWARE_ELEMENTS[0];
    assert!((element.properties)(1500, VideoContent::Camera).contains("rate-control=cbr"));
    assert!((element.properties)(1500, VideoContent::Screen).contains("rate-control=abr"));
    assert!(element.is_screen_quality_governed);
  }

  /// #979: scrolled text in the 720p layer of a 1440p share (1.7 Mbit/s). VideoToolbox sent
  /// 9 Mbit/s of it at its fixed quality; held by the [`QualityGovernor`] it stays within the
  /// bitrate, and once the text stands still it gets its quality back. Hardware only, like above.
  #[test]
  fn scrolled_text_stays_within_its_screen_layers_bitrate() {
    const SCROLLED: u64 = 120;
    const SETTLED: u64 = 45;
    /// Then the text stands still: the encoder thread encodes its picture again once a second.
    const STILL: u64 = 3;
    let screen = VideoResolution {
      width: 2560,
      height: 1440,
    };
    let layer = screen_layers(
      screen,
      30,
      ScreenContentMode::Detail,
      &ScreenShareResolution::Standard,
    )[0];
    let Some(mut encoder) = GstreamerEncoder::new(&layer, VideoContent::Screen) else {
      assert!(
        std::env::var_os("TEAMCRAFT_REQUIRE_HARDWARE_ENCODER").is_none(),
        "no hardware encoder"
      );
      return;
    };
    let (width, height) = (usize::from(layer.width), usize::from(layer.height));
    let page = TextPage::new(width * 2, height * 2 + usize::try_from(SCROLLED).unwrap() + 2);
    let mut settled_bytes = 0;
    for index in 0..SCROLLED + STILL {
      // A pixel of the page per frame: half a pixel of the layer, as a screen shrunk to it.
      let offset = usize::try_from(index.min(SCROLLED)).unwrap();
      let timestamp_us = if index < SCROLLED {
        index * 33_333
      } else {
        SCROLLED * 33_333 + (index - SCROLLED + 1) * 1_000_000
      };
      let frame = page.shrunk(layer, offset, timestamp_us);
      let maybe_picture = encoder.encode(&frame, index == 0).unwrap();
      if let Some(picture) = maybe_picture
        && (SETTLED..SCROLLED).contains(&index)
      {
        settled_bytes += picture.data.len() as u64;
      }
    }
    let seconds = (SCROLLED - SETTLED) as f64 / 30.0;
    let share = settled_bytes as f64 * 8.0 / seconds / f64::from(layer.max_bitrate_bps);
    assert!(
      (0.25..1.2).contains(&share),
      "scrolled text sent {:.0} % of the layer's {} kbit/s",
      share * 100.0,
      layer.max_bitrate_bps / 1000
    );
    if let Some(governed) = &encoder.maybe_governed {
      let quality = governed.governor.quality;
      assert_eq!(quality, SCREEN_QUALITY, "still text gets its quality back");
    }
  }

  /// Text on a page twice a layer's size, shrunk to the layer (2x2 averages) as a screen is.
  struct TextPage {
    width: usize,
    luma: Vec<u8>,
  }

  impl TextPage {
    fn new(width: usize, height: usize) -> Self {
      const LINE: usize = 26;
      let mut luma = vec![235u8; width * height];
      let mut random = 0x2545_f491_4f6c_dd1du64;
      for top in (0..height.saturating_sub(LINE)).step_by(LINE) {
        for left in (40..width.saturating_sub(52)).step_by(12) {
          random ^= random << 13;
          random ^= random >> 7;
          random ^= random << 17;
          if random.is_multiple_of(7) {
            continue;
          }
          // A glyph of 2x2 blocks (a pixel of the layer each), 5 wide and 8 high.
          for row in 0..16 {
            for column in 0..10 {
              if (random >> (row / 2 * 5 + column / 2)) & 1 == 1 {
                luma[(top + 5 + row) * width + left + column] = 40;
              }
            }
          }
        }
      }
      Self { width, luma }
    }

    /// The layer's picture of the page scrolled by `offset` rows of the page.
    fn shrunk(&self, layer: VideoLayerInfo, offset: usize, timestamp_us: u64) -> VideoFrame {
      let (width, height) = (usize::from(layer.width), usize::from(layer.height));
      let mut data = Vec::with_capacity(width * height * 3 / 2);
      for y in 0..height {
        let top = (2 * y + offset) * self.width;
        for x in 0..width {
          let at = top + 2 * x;
          let sum = u16::from(self.luma[at])
            + u16::from(self.luma[at + 1])
            + u16::from(self.luma[at + self.width])
            + u16::from(self.luma[at + self.width + 1]);
          data.push(u8::try_from(sum / 4).unwrap());
        }
      }
      data.resize(width * height * 3 / 2, 128);
      let resolution = VideoResolution {
        width: layer.width,
        height: layer.height,
      };
      VideoFrame::new(resolution, timestamp_us, data).unwrap()
    }
  }

  /// Bits of a picture of scrolled text in a 720p layer at `quality`, as VideoToolbox sends it at
  /// 30 fps: 9 Mbit/s at 0.5, 3.4 at 0.25, 0.3 at 0 (`examples/screen_bench.rs`).
  fn scrolled_text_bits(quality: f64) -> f64 {
    let bitrate_bps = if quality >= 0.25 {
      3.4e6 * (3.9 * (quality - 0.25)).exp()
    } else {
      0.29e6 * (9.8 * quality).exp()
    };
    bitrate_bps / 30.0
  }

  /// What a governed encoder sends: `bits_at` gives a picture's bits at a quality; a new quality
  /// rebuilds the encoder, which starts with a keyframe followed by a larger picture. Gives the
  /// bits of the pictures from `measure_from` on and the number of changes.
  fn govern(
    governor: &mut QualityGovernor,
    timestamps_us: impl Iterator<Item = u64>,
    bits_at: impl Fn(f64) -> f64,
    measure_from: u64,
  ) -> (f64, u32) {
    let (mut bits, mut changes) = (0.0, 0);
    let mut since_rebuild = None;
    for timestamp_us in timestamps_us {
      let is_keyframe = since_rebuild == Some(0);
      let picture_bits = match since_rebuild {
        Some(0) => 400_000.0,
        Some(1) => 3.0 * bits_at(governor.quality),
        _ => bits_at(governor.quality),
      };
      since_rebuild = since_rebuild.map(|pictures| pictures + 1);
      if timestamp_us >= measure_from {
        bits += picture_bits;
      }
      let bytes = (picture_bits / 8.0) as usize;
      if let Some(quality) = governor.note_picture(timestamp_us, bytes, is_keyframe) {
        governor.set_quality(quality);
        changes += 1;
        since_rebuild = Some(0);
      }
    }
    (bits, changes)
  }

  /// #979: scrolled text five times over a layer's bitrate is held within it in a few changes
  /// (each costs a keyframe), and the quality comes back a second after the text stops.
  #[test]
  fn the_governor_holds_scrolled_text_at_its_bitrate_and_lets_still_text_sharpen() {
    const TARGET_BPS: u32 = 1_659_000;
    let mut governor = QualityGovernor::new(TARGET_BPS);
    assert_eq!(governor.quality, SCREEN_QUALITY);
    // Five seconds of scrolling at 30 fps; the last three measured.
    let frames = (0..150u64).map(|index| index * 33_333);
    let (bits, changes) = govern(&mut governor, frames, scrolled_text_bits, 2_000_000);
    let share = bits / 3.0 / f64::from(TARGET_BPS);
    assert!(
      (LOW_SHARE * 0.8..1.0).contains(&share),
      "{:.0} % of the bitrate at quality {:.3}",
      share * 100.0,
      governor.quality
    );
    assert!(changes <= 4, "{changes} changes");
    assert!(governor.quality < 0.25, "quality {}", governor.quality);
    // The text stops: a picture a second of next to nothing.
    let still = (1..=2u64).map(|second| 5_000_000 + second * 1_000_000);
    govern(&mut governor, still, |_| 2_000.0, 0);
    assert_eq!(governor.quality, SCREEN_QUALITY);
  }

  /// A still screen and one being typed on stay at the full quality: they need a fraction of
  /// their bitrate, and requested keyframes (with the larger picture after them) don't count.
  #[test]
  fn the_governor_keeps_a_still_or_typed_on_screen_at_its_quality() {
    let mut governor = QualityGovernor::new(1_659_000);
    // A character typed every 110 ms, the cursor blinking; a keyframe every other second.
    let mut timestamp_us = 0;
    for index in 0..100u64 {
      let is_keyframe = index % 18 == 0;
      let bytes = match index % 18 {
        0 => 120_000,
        1 => 20_000,
        _ => 1_500,
      };
      assert_eq!(governor.note_picture(timestamp_us, bytes, is_keyframe), None);
      timestamp_us += 110_000;
    }
    // Then nothing changes: the picture again once a second.
    for _ in 0..5 {
      timestamp_us += 1_000_000;
      assert_eq!(governor.note_picture(timestamp_us, 300, false), None);
    }
    assert_eq!(governor.quality, SCREEN_QUALITY);
  }

  /// A layer over its bitrate goes down at once; one that just went down doesn't come straight
  /// back up, even far below its bitrate.
  #[test]
  fn the_governor_falls_at_once_and_rises_after_a_second() {
    let mut governor = QualityGovernor::new(1_000_000);
    let mut changes = Vec::new();
    for index in 0..5u64 {
      // 2 Mbit/s at 30 fps.
      if let Some(quality) = governor.note_picture(index * 33_333, 8_333, false) {
        governor.set_quality(quality);
        changes.push((index, quality));
      }
    }
    assert_eq!(changes.len(), 1, "{changes:?}");
    let (index, quality) = changes[0];
    assert_eq!(index, 4, "decided on four measured pictures");
    assert!((quality - (SCREEN_QUALITY - 2f64.ln() / FALL_SLOPE)).abs() < 1e-3);
    // Now far below: the quality holds for a second after the change.
    let mut timestamp_us = 4 * 33_333;
    let mut maybe_rise = None;
    while maybe_rise.is_none() {
      timestamp_us += 33_333;
      maybe_rise = governor.note_picture(timestamp_us, 100, false);
      assert!(timestamp_us < 3_000_000, "never rose");
    }
    assert!(timestamp_us >= 4 * 33_333 + MIN_RISE_INTERVAL_US);
    assert_eq!(maybe_rise, Some(SCREEN_QUALITY));
  }

  #[test]
  fn a_governed_pipeline_names_its_quality_in_every_locale() {
    let element = HardwareElement {
      name: "vtenc_h264",
      profile: "baseline",
      properties: |bitrate_kbps, _content| format!("bitrate={bitrate_kbps}"),
      is_screen_quality_governed: true,
    };
    let layer = simulcast_layers(HD, 30, 1_659_000, 1)[0];
    let governed = pipeline_description(&element, &layer, VideoContent::Screen, Some(0.125));
    assert!(
      governed.contains("vtenc_h264 bitrate=1659 quality=0.125 !"),
      "{governed}"
    );
    let plain = pipeline_description(&element, &layer, VideoContent::Camera, None);
    assert!(!plain.contains("quality"), "{plain}");
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

  fn luma_psnr(a: &VideoFrame, b: &VideoFrame) -> f64 {
    let (ya, yb) = (a.planes().0, b.planes().0);
    let mse = ya
      .iter()
      .zip(yb)
      .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
      .sum::<f64>()
      / ya.len() as f64;
    10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10()
  }
}
