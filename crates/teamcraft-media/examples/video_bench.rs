//! Per-frame costs of the video send path at 720p: simulcast encoding, the effects compositor and,
//! given a model, segmentation.
//!
//! ```sh
//! cargo run --release -p teamcraft-media --example video_bench --features native,segmentation
//! TEAMCRAFT_SEGMENTATION_MODEL=selfie_segmentation_landscape.onnx cargo run --release ...
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use teamcraft_media::effects::{
  BackgroundEffect, BackgroundImage, BlurStrength, Compositor, EffectsConfig, EllipseSegmenter,
  PersonSegmenter, SegmentationModel, TractSegmenter,
};
use teamcraft_media::video::{VideoFrame, VideoResolution, simulcast_layers};
use teamcraft_media::video_capture::test_pattern;
use teamcraft_media::video_codec::{OpenH264Encoder, SimulcastEncoder, VideoContent, VideoEncoder};
use tracing::info;

const HD: VideoResolution = VideoResolution {
  width: 1280,
  height: 720,
};
const FRAMES: u64 = 120;

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt().with_target(false).init();
  let frames: Vec<VideoFrame> = (0..FRAMES)
    .map(|index| test_pattern(HD, index, index * 33_333))
    .collect::<Result<_, _>>()?;

  let layers = simulcast_layers(HD, 30, 1_500_000, 3);
  let mut simulcast = SimulcastEncoder::new(&layers, |layer| {
    Ok(Box::new(OpenH264Encoder::new(layer, VideoContent::Camera)?) as Box<dyn VideoEncoder>)
  })?;
  report(
    "H.264 simulcast encode (3 layers, software)",
    time(&frames, |frame| {
      simulcast.encode(frame).map(drop).context("encode")
    })?,
  );

  let mut compositor = pollster::block_on(Compositor::new(EffectsConfig::default()))?;
  let mask = EllipseSegmenter {
    width: 256,
    height: 144,
  }
  .segment(&frames[0])?;
  let image = Arc::new(BackgroundImage::new(
    1920,
    1080,
    (0..1920 * 1080)
      .flat_map(|i: u32| [(i % 251) as u8, 90, 160, 255])
      .collect(),
  )?);
  for (name, effect) in [
    ("effects: light blur", BackgroundEffect::Blur(BlurStrength::Light)),
    (
      "effects: strong blur",
      BackgroundEffect::Blur(BlurStrength::Strong),
    ),
    ("effects: background image", BackgroundEffect::Image(image)),
  ] {
    report(
      name,
      time(&frames, |frame| {
        pollster::block_on(compositor.apply(frame, &mask, &effect))
          .map(drop)
          .context("effects")
      })?,
    );
  }

  match std::env::var("TEAMCRAFT_SEGMENTATION_MODEL") {
    Ok(path) => {
      let model = std::fs::read(&path).with_context(|| format!("reading {path}"))?;
      let mut segmenter = TractSegmenter::new(&model, SegmentationModel::MEDIAPIPE_LANDSCAPE)?;
      report(
        "segmentation (tract, CPU)",
        time(&frames, |frame| {
          segmenter.segment(frame).map(drop).context("segment")
        })?,
      );
    },
    Err(_) => info!("set TEAMCRAFT_SEGMENTATION_MODEL to time segmentation"),
  }
  Ok(())
}

/// Average and worst time per frame, after one warm-up frame.
fn time(
  frames: &[VideoFrame],
  mut work: impl FnMut(&VideoFrame) -> anyhow::Result<()>,
) -> anyhow::Result<(Duration, Duration)> {
  work(&frames[0])?;
  let mut worst = Duration::ZERO;
  let start = Instant::now();
  for frame in &frames[1..] {
    let frame_start = Instant::now();
    work(frame)?;
    worst = worst.max(frame_start.elapsed());
  }
  Ok((start.elapsed() / (frames.len() as u32 - 1), worst))
}

fn report(name: &str, (average, worst): (Duration, Duration)) {
  info!(
    average_ms = format!("{:.2}", average.as_secs_f64() * 1000.0),
    worst_ms = format!("{:.2}", worst.as_secs_f64() * 1000.0),
    "{name}"
  );
}
