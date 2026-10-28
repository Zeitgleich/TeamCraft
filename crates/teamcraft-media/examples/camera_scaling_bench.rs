//! CPU time and latency of the desktop camera send path when the camera delivers more than the
//! call sends (issue #537): 4K and 1080p cameras shrunk to 720p, against a 720p camera, with and
//! without a background effect, with OpenH264 and (feature `gstreamer`) the hardware encoder.
//!
//! The frames are synthetic: the moving test pattern with a little sensor noise, as packed YUV
//! (`yuvs`, what AVFoundation hands the macOS app), so no camera is needed. Each stage is timed on
//! its own, then the whole chain per camera size at 30 fps: the copy out of the camera's buffer,
//! YUYV → I420, `VideoFrame::scaled` to the send size, the effect (segmentation + compositor) and
//! the three simulcast layers, whose encoders scale the lower layers themselves.
//!
//! ```sh
//! crates/teamcraft-media/models/fetch.sh
//! TEAMCRAFT_SEGMENTATION_MODEL=crates/teamcraft-media/models/selfie_segmentation_landscape.onnx \
//!   cargo run --release -p teamcraft-media --example camera_scaling_bench \
//!   --features openh264-source,segmentation,gstreamer
//! ```
//!
//! Without a model the effect's mask comes from a fixed ellipse (the compositor still runs).
//! "CPU" is the process's CPU time (all threads: encoder threads, GPU driver) per frame, and
//! "% core" that time against the 33 ms a frame may take at 30 fps.

use std::time::{Duration, Instant};

use anyhow::Context;
use teamcraft_media::capture_convert::{PixelFormat, RawPicture};
use teamcraft_media::effects::{
  BackgroundEffect, BlurStrength, Compositor, EffectsConfig, EffectsPipeline, EllipseSegmenter,
  PersonSegmenter, SegmentationModel, TractSegmenter,
};
use teamcraft_media::video::{VideoResolution, fit_within, simulcast_layers};
use teamcraft_media::video_capture::test_pattern;
use teamcraft_media::video_codec::{SimulcastEncoder, VideoContent};
use teamcraft_media::video_pipeline::{EncoderFactory, FrameFilter, software_encoders};
use tracing::info;

/// The call's camera send target.
const SEND: VideoResolution = VideoResolution {
  width: 1280,
  height: 720,
};
const SOURCES: [(&str, VideoResolution); 3] = [
  (
    "4K",
    VideoResolution {
      width: 3840,
      height: 2160,
    },
  ),
  (
    "1080p",
    VideoResolution {
      width: 1920,
      height: 1080,
    },
  ),
  ("720p", SEND),
];
/// Distinct source pictures, reused in turn (16 4K pictures in YUYV are 265 MB).
const RING: u64 = 16;
/// Frames per measurement: 10 s at 30 fps.
const FRAMES: u64 = 300;
const FRAME_INTERVAL: Duration = Duration::from_micros(33_333);
const FULL_BITRATE_BPS: u32 = 1_500_000;

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt().with_target(false).init();
  let maybe_model = match std::env::var("TEAMCRAFT_SEGMENTATION_MODEL") {
    Ok(path) => Some(std::fs::read(&path).with_context(|| format!("reading {path}"))?),
    Err(_) => {
      info!("TEAMCRAFT_SEGMENTATION_MODEL not set: the effect's mask is a fixed ellipse");
      None
    },
  };
  let encoders = encoder_choices();
  let mut stages = Vec::new();
  let mut chains = Vec::new();

  for (name, source) in SOURCES {
    let pictures = (0..RING)
      .map(|index| yuyv_picture(source, index))
      .collect::<anyhow::Result<Vec<_>>>()?;
    let raw = |index: u64| RawPicture {
      format: PixelFormat::Yuyv,
      width: u32::from(source.width),
      height: u32::from(source.height),
      stride: 0,
      data: &pictures[(index % RING) as usize],
    };
    let target = fit_within(source, SEND);

    stages.push(measure(
      &format!("{name}: copy out of the camera buffer"),
      false,
      |index| {
        std::hint::black_box(raw(index).data.to_vec());
        Ok(())
      },
    )?);
    stages.push(measure(&format!("{name}: YUYV -> I420"), false, |index| {
      std::hint::black_box(raw(index).to_i420(index * 33_333)?);
      Ok(())
    })?);
    if target != source {
      let frames = (0..RING)
        .map(|index| raw(index).to_i420(0))
        .collect::<Result<Vec<_>, _>>()?;
      stages.push(measure(
        &format!("{name}: VideoFrame::scaled to {}x{}", target.width, target.height),
        false,
        |index| {
          std::hint::black_box(frames[(index % RING) as usize].scaled(target)?);
          Ok(())
        },
      )?);
    }

    for (encoder_name, factory) in &encoders {
      for effect in [false, true] {
        let mut maybe_filter = if effect {
          Some(effect_filter(maybe_model.as_deref())?)
        } else {
          None
        };
        let layers = simulcast_layers(target, 30, FULL_BITRATE_BPS, 3);
        let mut simulcast = SimulcastEncoder::new(&layers, |layer| factory(layer, VideoContent::Camera))?;
        let label = format!(
          "{name} chain, {}, {encoder_name}",
          if effect { "blur" } else { "no effect" }
        );
        chains.push(measure(&label, true, |index| {
          let timestamp_us = index * 33_333;
          let copied = raw(index).data.to_vec();
          let picture = RawPicture {
            data: &copied,
            ..raw(index)
          };
          let frame = picture.to_i420(timestamp_us)?;
          let frame = if target == frame.resolution() {
            frame
          } else {
            frame.scaled(target)?
          };
          let frame = match &mut maybe_filter {
            Some(filter) => filter(frame),
            None => frame,
          };
          std::hint::black_box(simulcast.encode(&frame)?);
          Ok(())
        })?);
      }
    }
  }

  // What the encoders do per frame besides encoding, at the send size: the lower layers scaled
  // from 720p, the top layer copied (`scaled` to the same size clones), and for the hardware
  // encoder the I420 → NV12 repacking (inside the encode rows above, not added to them).
  let frames = (0..RING)
    .map(|index| test_pattern(SEND, index, 0))
    .collect::<Result<Vec<_>, _>>()?;
  let layers = simulcast_layers(SEND, 30, FULL_BITRATE_BPS, 3);
  stages.push(measure(
    "720p: encoder-side layer scaling (3 layers)",
    false,
    |index| {
      let frame = &frames[(index % RING) as usize];
      for layer in &layers {
        std::hint::black_box(frame.scaled(VideoResolution {
          width: layer.width,
          height: layer.height,
        })?);
      }
      Ok(())
    },
  )?);
  let frame = &frames[0];
  stages.push(measure("720p: compositor only (light blur, GPU)", false, {
    let mut compositor = pollster::block_on(Compositor::new(EffectsConfig::default()))?;
    let mask = EllipseSegmenter {
      width: 256,
      height: 144,
    }
    .segment(frame)?;
    move |_| {
      pollster::block_on(compositor.apply(frame, &mask, &BackgroundEffect::Blur(BlurStrength::Light)))?;
      Ok(())
    }
  })?);
  if let Some(model) = &maybe_model {
    let mut segmenter = TractSegmenter::new(model, SegmentationModel::MEDIAPIPE_LANDSCAPE)?;
    stages.push(measure("720p: segmentation only (tract, CPU)", false, |_| {
      std::hint::black_box(segmenter.segment(frame)?);
      Ok(())
    })?);
  }

  info!("| Stage | Average | Worst | CPU per frame | % of a core at 30 fps |");
  for row in stages.iter().chain(&chains) {
    info!(
      "| {} | {:.2} ms | {:.2} ms | {:.2} ms | {:.1} % |",
      row.name,
      millis(row.average),
      millis(row.worst),
      millis(row.cpu_per_frame),
      row.cpu_per_frame.as_secs_f64() / FRAME_INTERVAL.as_secs_f64() * 100.0
    );
  }
  Ok(())
}

/// OpenH264, and the hardware encoder when GStreamer has one.
fn encoder_choices() -> Vec<(&'static str, EncoderFactory)> {
  #[allow(unused_mut)]
  let mut encoders = vec![("OpenH264", software_encoders())];
  #[cfg(feature = "gstreamer")]
  {
    use teamcraft_media::video_gstreamer::GstreamerEncoder;
    let probe = simulcast_layers(SEND, 30, FULL_BITRATE_BPS, 1);
    match GstreamerEncoder::new(&probe[0], VideoContent::Camera) {
      Some(encoder) => {
        use teamcraft_media::video_codec::VideoEncoder;
        info!(element = encoder.name(), "hardware encoder");
        encoders.push(("hardware", teamcraft_media::video_pipeline::preferred_encoders()));
      },
      None => info!("no hardware encoder through GStreamer"),
    }
  }
  encoders
}

/// The effect as the encoder thread runs it: segmentation, then the compositor's light blur.
fn effect_filter(maybe_model: Option<&[u8]>) -> anyhow::Result<FrameFilter> {
  let segmenter: Box<dyn PersonSegmenter> = match maybe_model {
    Some(model) => Box::new(TractSegmenter::new(
      model,
      SegmentationModel::MEDIAPIPE_LANDSCAPE,
    )?),
    None => Box::new(EllipseSegmenter {
      width: 256,
      height: 144,
    }),
  };
  let compositor = pollster::block_on(Compositor::new(EffectsConfig::default()))?;
  let pipeline = EffectsPipeline::new(segmenter, compositor, BackgroundEffect::Blur(BlurStrength::Light));
  Ok(pipeline.into_frame_filter().0)
}

/// The test pattern at `resolution` as packed YUYV, with a little noise on the luma (a camera's
/// sensor noise; a noiseless picture is unrealistically cheap to encode).
fn yuyv_picture(resolution: VideoResolution, index: u64) -> anyhow::Result<Vec<u8>> {
  let frame = test_pattern(resolution, index, 0)?;
  let (y, u, v) = frame.planes();
  let (width, height) = (usize::from(resolution.width), usize::from(resolution.height));
  let mut noise = 0x9e37_79b9_u32.wrapping_mul(index as u32 + 1);
  let mut noisy = |luma: u8| {
    noise ^= noise << 13;
    noise ^= noise >> 17;
    noise ^= noise << 5;
    luma.saturating_add((noise & 7) as u8).saturating_sub(4)
  };
  let mut packed = Vec::with_capacity(width * height * 2);
  for row in 0..height {
    let chroma_row = (row / 2) * (width / 2);
    for pair in 0..width / 2 {
      packed.extend([
        noisy(y[row * width + pair * 2]),
        u[chroma_row + pair],
        noisy(y[row * width + pair * 2 + 1]),
        v[chroma_row + pair],
      ]);
    }
  }
  Ok(packed)
}

struct Row {
  name: String,
  average: Duration,
  worst: Duration,
  cpu_per_frame: Duration,
}

/// Runs `work` for [`FRAMES`] frames after a few warm-up frames: wall time per frame (latency)
/// and the process's CPU time per frame. `paced` spaces the frames 33 ms apart like a camera, so
/// threads that work between frames (hardware encoders, the GPU) count as they would in a call.
fn measure(name: &str, paced: bool, mut work: impl FnMut(u64) -> anyhow::Result<()>) -> anyhow::Result<Row> {
  for index in 0..5 {
    work(index)?;
  }
  let mut worst = Duration::ZERO;
  let mut total = Duration::ZERO;
  let cpu_start = cpu_time();
  let start = Instant::now();
  for index in 0..FRAMES {
    if paced {
      let due = start + FRAME_INTERVAL * index as u32;
      if let Some(wait) = due.checked_duration_since(Instant::now()) {
        std::thread::sleep(wait);
      }
    }
    let frame_start = Instant::now();
    work(index + 5)?;
    let took = frame_start.elapsed();
    worst = worst.max(took);
    total += took;
  }
  let row = Row {
    name: name.to_owned(),
    average: total / FRAMES as u32,
    worst,
    cpu_per_frame: cpu_time().saturating_sub(cpu_start) / FRAMES as u32,
  };
  info!(
    average_ms = format!("{:.2}", millis(row.average)),
    worst_ms = format!("{:.2}", millis(row.worst)),
    cpu_ms = format!("{:.2}", millis(row.cpu_per_frame)),
    "{name}"
  );
  Ok(row)
}

fn millis(duration: Duration) -> f64 {
  duration.as_secs_f64() * 1000.0
}

/// User plus system CPU time of the whole process so far.
#[cfg(unix)]
fn cpu_time() -> Duration {
  use nix::sys::resource::{UsageWho, getrusage};
  getrusage(UsageWho::RUSAGE_SELF).map_or(Duration::ZERO, |usage| {
    let to_duration = |time: nix::sys::time::TimeVal| {
      Duration::new(time.tv_sec().max(0) as u64, (time.tv_usec().max(0) * 1000) as u32)
    };
    to_duration(usage.user_time()) + to_duration(usage.system_time())
  })
}

/// Not measured outside Unix: the CPU column stays 0.
#[cfg(not(unix))]
fn cpu_time() -> Duration {
  Duration::ZERO
}
