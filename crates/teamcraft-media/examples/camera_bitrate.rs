//! What each simulcast layer of a camera really sends, against its target (issue #967: a 720p
//! layer meant for 1.5 Mbit/s sent 155 to 306 kbit/s through VideoToolbox).
//!
//! Frames are encoded as they come, in real time, like the call's encoder thread does: from the
//! camera (macOS), or from a calm synthetic picture of a person in a room with a camera's noise.
//!
//! ```sh
//! cargo run --release -p teamcraft-media --features avfoundation,gstreamer,openh264-source \
//!   --example camera_bitrate -- camera [--seconds 10] [--max 720] [--encoder software]
//! cargo run --release -p teamcraft-media --features gstreamer,openh264-source \
//!   --example camera_bitrate -- calm [--seconds 10] [--size 1280x720] [--max 720] \
//!   [--encoder software] [--noise 1]
//! ```
//!
//! `--encoder preferred` (the default) encodes as the call does: the hardware encoder where there
//! is one (VideoToolbox, VA-API), OpenH264 otherwise. The first second (the keyframes, the rate
//! control settling) is left out of the rates. `--noise` is the calm picture's sensor noise (the
//! largest offset of a pixel, default 1: a camera that denoises, like a Mac's in daylight) and
//! `--size` its size (default 1920x1080). The layers are the call's (`video::camera_layers`);
//! `--max` caps them like the user's setting does (1080, 720 or 360 lines).
//!
//! The report is logged as a table, a row per layer. The PSNR column decodes a sample of each
//! layer's pictures with OpenH264 and compares them with what the layer's encoder was given. The
//! encoding's cost is the process's CPU time (all threads: GStreamer's and VideoToolbox's too)
//! and the wall time while the simulcast encoder works on a frame, after the warm-up. The
//! numbers of #967 are in docs/performance.md ("What each camera layer sends").

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use teamcraft_media::video::{
  CAMERA_MAX_RESOLUTION, VideoFrame, VideoResolution, camera_layers, camera_limit,
};
use teamcraft_media::video_capture::{CaptureFormat, VideoCapture, frame_queue};
use teamcraft_media::video_codec::{OpenH264Decoder, SimulcastEncoder, VideoContent, VideoDecoder};
use teamcraft_media::video_pipeline::{EncoderFactory, preferred_encoders, software_encoders};
use teamcraft_proto::VideoLayerInfo;
use tracing::info;

const FRAMERATE: u8 = 30;
/// Left out of the rates: the first keyframes and the rate control settling.
const WARM_UP: Duration = Duration::from_secs(1);
/// Every this many decoded pictures of a layer is compared with its source (PSNR).
const PSNR_EVERY: u64 = 10;
/// An encoder may give a frame's picture a frame or two later: the best match of the last few
/// sources counts.
const RECENT_SOURCES: usize = 3;

/// What one layer sent.
#[derive(Debug, Default)]
struct LayerTally {
  pictures: u64,
  bytes: u64,
  keyframes: u64,
  keyframe_bytes: u64,
  /// After [`WARM_UP`].
  steady_pictures: u64,
  steady_bytes: u64,
  /// Luma PSNR (dB) of sampled pictures after [`WARM_UP`], decoded with OpenH264.
  psnr_sum: f64,
  psnr_samples: u64,
}

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt().with_target(false).init();
  let mut arguments = std::env::args().skip(1);
  let source = arguments.next().unwrap_or_else(|| "calm".into());
  let mut seconds = 10u64;
  let mut size = CAMERA_MAX_RESOLUTION;
  let mut maybe_max_lines = None;
  let mut encoder = "preferred".to_owned();
  let mut noise = 1.0;
  while let Some(argument) = arguments.next() {
    match (argument.as_str(), arguments.next()) {
      ("--seconds", Some(value)) => seconds = value.parse().context("--seconds takes a number")?,
      ("--size", Some(value)) => size = parse_size(&value)?,
      ("--encoder", Some(value)) => encoder = value,
      ("--noise", Some(value)) => noise = value.parse().context("--noise takes a number")?,
      ("--max", Some(value)) => maybe_max_lines = Some(value.parse().context("--max takes lines")?),
      (other, _) => bail!("unknown argument {other}"),
    }
  }
  let encoders: EncoderFactory = match encoder.as_str() {
    "preferred" => preferred_encoders(),
    "software" => software_encoders(),
    other => bail!("unknown encoder {other}; use preferred or software"),
  };
  let limit = camera_limit(maybe_max_lines);
  let format = CaptureFormat {
    resolution: size,
    framerate: FRAMERATE,
  };
  let duration = Duration::from_secs(seconds);
  let run = match source.as_str() {
    "camera" => run_camera(limit, &encoders, duration)?,
    "calm" => run_calm(format, limit, &encoders, duration, noise)?,
    other => bail!("unknown source {other}; use camera or calm"),
  };
  report(&run);
  Ok(())
}

fn parse_size(value: &str) -> anyhow::Result<VideoResolution> {
  let (width, height) = value.split_once('x').context("--size takes WIDTHxHEIGHT")?;
  Ok(VideoResolution {
    width: width.parse()?,
    height: height.parse()?,
  })
}

/// What a run gives: the layers, what each sent, how long it ran, the encoders and their cost.
struct Run {
  layers: Vec<VideoLayerInfo>,
  tallies: BTreeMap<u8, LayerTally>,
  elapsed: Duration,
  encoder_names: String,
  cost: EncodeCost,
}

/// What encoding the frames after the warm-up took.
#[derive(Debug, Default)]
struct EncodeCost {
  frames: u32,
  cpu: Duration,
  wall: Duration,
}

#[cfg(all(target_os = "macos", feature = "avfoundation"))]
fn run_camera(limit: VideoResolution, encoders: &EncoderFactory, duration: Duration) -> anyhow::Result<Run> {
  let format = CaptureFormat {
    resolution: limit,
    framerate: FRAMERATE,
  };
  let mut camera = teamcraft_media::capture_nokhwa::NokhwaCamera::open(None, format).context("camera")?;
  let (sender, received) = frame_queue();
  camera.start(sender).context("starting the camera")?;
  let first = received
    .recv_timeout(Duration::from_secs(10))
    .context("no picture from the camera")?;
  info!(resolution = ?first.resolution(), "first picture");
  let mut encoding = Encoding::new(first.resolution(), limit, encoders)?;
  encoding.encode(&first)?;
  while encoding.started.elapsed() < duration {
    if let Ok(frame) = received.recv_timeout(Duration::from_millis(500)) {
      encoding.encode(&frame)?;
    }
  }
  camera.stop();
  Ok(encoding.finish())
}

#[cfg(not(all(target_os = "macos", feature = "avfoundation")))]
fn run_camera(
  _limit: VideoResolution,
  _encoders: &EncoderFactory,
  _duration: Duration,
) -> anyhow::Result<Run> {
  bail!("the camera source needs macOS and the avfoundation feature")
}

/// The calm picture at the camera's rate, in real time.
fn run_calm(
  format: CaptureFormat,
  limit: VideoResolution,
  encoders: &EncoderFactory,
  duration: Duration,
  noise: f64,
) -> anyhow::Result<Run> {
  let mut encoding = Encoding::new(format.resolution, limit, encoders)?;
  let interval = Duration::from_secs(1) / u32::from(format.framerate);
  let mut index = 0u64;
  while encoding.started.elapsed() < duration {
    let due = encoding.started + interval * u32::try_from(index)?;
    if let Some(wait) = due.checked_duration_since(Instant::now()) {
      std::thread::sleep(wait);
    }
    let timestamp_us = u64::try_from(encoding.started.elapsed().as_micros())?;
    encoding.encode(&calm_picture(format.resolution, index, timestamp_us, noise)?)?;
    index += 1;
  }
  Ok(encoding.finish())
}

/// One track's encoders as the call runs them, and what each layer sent.
struct Encoding {
  layers: Vec<VideoLayerInfo>,
  simulcast: SimulcastEncoder,
  tallies: BTreeMap<u8, LayerTally>,
  decoders: BTreeMap<u8, OpenH264Decoder>,
  recent: VecDeque<VideoFrame>,
  started: Instant,
  cost: EncodeCost,
}

impl Encoding {
  fn new(source: VideoResolution, limit: VideoResolution, encoders: &EncoderFactory) -> anyhow::Result<Self> {
    let layers = camera_layers(source, FRAMERATE, limit);
    for layer in &layers {
      info!(
        layer = layer.layer,
        size = format!("{}x{}", layer.width, layer.height),
        target_kbps = layer.max_bitrate_bps / 1000,
        "layer"
      );
    }
    let simulcast = SimulcastEncoder::new(&layers, |layer| encoders(layer, VideoContent::Camera))?;
    let decoders = layers
      .iter()
      .map(|layer| Ok((layer.layer, OpenH264Decoder::new()?)))
      .collect::<anyhow::Result<_>>()?;
    Ok(Self {
      layers,
      simulcast,
      tallies: BTreeMap::new(),
      decoders,
      recent: VecDeque::new(),
      started: Instant::now(),
      cost: EncodeCost::default(),
    })
  }

  fn encode(&mut self, frame: &VideoFrame) -> anyhow::Result<()> {
    let is_steady = self.started.elapsed() >= WARM_UP;
    if self.recent.len() == RECENT_SOURCES {
      self.recent.pop_front();
    }
    self.recent.push_back(frame.clone());
    let (cpu_before, wall_before) = (cpu_time(), Instant::now());
    let pictures = self.simulcast.encode(frame)?;
    if is_steady {
      self.cost.frames += 1;
      self.cost.cpu += cpu_time().saturating_sub(cpu_before);
      self.cost.wall += wall_before.elapsed();
    }
    for picture in pictures {
      let tally = self.tallies.entry(picture.layer.value()).or_default();
      let maybe_decoded = match self.decoders.get_mut(&picture.layer.value()) {
        Some(decoder) => decoder.decode(&picture.data, frame.timestamp_us())?,
        None => None,
      };
      if let Some(decoded) = maybe_decoded.filter(|_| is_steady && tally.pictures.is_multiple_of(PSNR_EVERY))
      {
        let mut best = 0.0f64;
        for source in &self.recent {
          let reference = layer_input(source, &self.layers, decoded.resolution())?;
          best = best.max(luma_psnr(&reference, &decoded));
        }
        tally.psnr_sum += best;
        tally.psnr_samples += 1;
      }
      let bytes = picture.data.len() as u64;
      tally.pictures += 1;
      tally.bytes += bytes;
      if picture.is_keyframe {
        tally.keyframes += 1;
        tally.keyframe_bytes += bytes;
      }
      if is_steady {
        tally.steady_pictures += 1;
        tally.steady_bytes += bytes;
      }
    }
    Ok(())
  }

  fn finish(self) -> Run {
    Run {
      elapsed: self.started.elapsed(),
      encoder_names: self.simulcast.encoder_names(),
      layers: self.layers,
      tallies: self.tallies,
      cost: self.cost,
    }
  }
}

fn report(run: &Run) {
  let Run {
    layers,
    tallies,
    elapsed,
    encoder_names,
    cost,
  } = run;
  let steady_seconds = elapsed.saturating_sub(WARM_UP).as_secs_f64().max(0.001);
  let per_frame = |total: Duration| total.as_secs_f64() * 1000.0 / f64::from(cost.frames.max(1));
  info!(
    "encoder: {encoder_names}, {:.1} s; encoding {:.1} ms CPU, {:.1} ms wall per frame",
    elapsed.as_secs_f64(),
    per_frame(cost.cpu),
    per_frame(cost.wall)
  );
  info!(
    "{:<6} {:>10} {:>10} {:>10} {:>7} {:>6} {:>10} {:>11} {:>8}",
    "layer", "size", "target", "sent", "share", "fps", "keyframes", "avg P bytes", "PSNR"
  );
  for layer in layers {
    let tally = tallies.get(&layer.layer);
    let steady_bytes = tally.map_or(0, |tally| tally.steady_bytes);
    let steady_pictures = tally.map_or(0, |tally| tally.steady_pictures);
    let sent_kbps = steady_bytes as f64 * 8.0 / steady_seconds / 1000.0;
    let target_kbps = f64::from(layer.max_bitrate_bps) / 1000.0;
    let (keyframes, keyframe_bytes, pictures, bytes) = tally.map_or((0, 0, 0, 0), |tally| {
      (tally.keyframes, tally.keyframe_bytes, tally.pictures, tally.bytes)
    });
    let deltas = pictures.saturating_sub(keyframes).max(1);
    let psnr = tally.filter(|tally| tally.psnr_samples > 0).map_or_else(
      || "-".to_owned(),
      |tally| format!("{:.1} dB", tally.psnr_sum / tally.psnr_samples as f64),
    );
    info!(
      "{:<6} {:>10} {:>6.0} kb/s {:>6.0} kb/s {:>6.0}% {:>6.1} {:>10} {:>11.0} {:>8}",
      layer.layer,
      format!("{}x{}", layer.width, layer.height),
      target_kbps,
      sent_kbps,
      sent_kbps / target_kbps * 100.0,
      steady_pictures as f64 / steady_seconds,
      keyframes,
      bytes.saturating_sub(keyframe_bytes) as f64 / deltas as f64,
      psnr,
    );
  }
}

/// What the simulcast encoder gives the layer of `size`: `source` shrunk to it, by way of the
/// layer twice its size where there is one (`SimulcastEncoder::encode`). The PSNR compares with
/// that, not with another way of shrinking.
fn layer_input(
  source: &VideoFrame,
  layers: &[VideoLayerInfo],
  size: VideoResolution,
) -> anyhow::Result<VideoFrame> {
  let is_double = |layer: &&VideoLayerInfo| layer.width == size.width * 2 && layer.height == size.height * 2;
  let shrunk = match layers.iter().find(is_double) {
    Some(double) => {
      let double = VideoResolution {
        width: double.width,
        height: double.height,
      };
      layer_input(source, layers, double)?.scaled(size)?
    },
    None => source.scaled(size)?,
  };
  Ok(shrunk)
}

/// The process's CPU time so far, over all threads.
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

/// Not measured outside Unix: the CPU time stays 0.
#[cfg(not(unix))]
fn cpu_time() -> Duration {
  Duration::ZERO
}

fn luma_psnr(source: &VideoFrame, decoded: &VideoFrame) -> f64 {
  let (a, b) = (source.planes().0, decoded.planes().0);
  let squared_error = a
    .iter()
    .zip(b)
    .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
    .sum::<f64>()
    / a.len().max(1) as f64;
  10.0 * (255.0f64.powi(2) / squared_error.max(1e-9)).log10()
}

/// A calm camera picture: a room (a wall with a soft light gradient, a bookshelf, a window) and a
/// person who barely moves, with a camera's sensor noise: what a video call mostly shows.
fn calm_picture(
  resolution: VideoResolution,
  index: u64,
  timestamp_us: u64,
  noise_amplitude: f64,
) -> anyhow::Result<VideoFrame> {
  let (width, height) = (usize::from(resolution.width), usize::from(resolution.height));
  let scale = |value: f64, side: usize| value * side as f64;
  // The head sways by a few pixels, slowly (about a second per sway).
  let seconds = timestamp_us as f64 / 1e6;
  let sway = (seconds * 1.3).sin() * scale(0.004, width);
  let nod = (seconds * 0.9).cos() * scale(0.003, height);
  let (head_x, head_y) = (scale(0.5, width) + sway, scale(0.42, height) + nod);
  let (head_rx, head_ry) = (scale(0.09, width), scale(0.2, height));
  let mut noise = Noise(index.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
  let mut data = Vec::with_capacity(width * height * 3 / 2);
  for y in 0..height {
    for x in 0..width {
      let (fx, fy) = (x as f64 / width as f64, y as f64 / height as f64);
      let dx = (x as f64 - head_x) / head_rx;
      let dy = (y as f64 - head_y) / head_ry;
      let body = fy > 0.62 && (fx - 0.5 - sway / width as f64).abs() < 0.22 - (1.0 - fy) * 0.1;
      let luma = if dx * dx + dy * dy <= 1.0 {
        // Skin with soft shading and a little texture.
        150.0 - dx * 25.0 + ((x / 3 + y / 5) % 7) as f64
      } else if body {
        // A sweater with a knit pattern.
        70.0 + ((x / 4 + y / 4) % 3) as f64 * 6.0
      } else if fx < 0.3 && fy > 0.15 && fy < 0.85 {
        // A bookshelf: book spines of different shades.
        let book = (x * 37 / width.max(1)) % 9;
        let shelf = (fy * 5.0).fract() < 0.06;
        if shelf { 50.0 } else { 60.0 + book as f64 * 14.0 }
      } else if fx > 0.72 && fx < 0.95 && fy > 0.1 && fy < 0.55 {
        // A bright window with its frame.
        if (fx - 0.835).abs() < 0.006 || (fy - 0.325).abs() < 0.01 {
          120.0
        } else {
          225.0
        }
      } else {
        // The wall, lit from the window.
        110.0 + fx * 40.0 - fy * 20.0
      };
      let noisy = luma + noise.next_offset(noise_amplitude);
      data.push(noisy.clamp(16.0, 235.0) as u8);
    }
  }
  for plane in [0, 1] {
    for y in 0..height / 2 {
      for x in 0..width / 2 {
        let (fx, fy) = (x as f64 * 2.0 / width as f64, y as f64 * 2.0 / height as f64);
        let warm = if plane == 0 { 118.0 } else { 140.0 };
        let base = if fx < 0.3 && fy > 0.15 && fy < 0.85 {
          128.0 + (((x * 74 / width.max(1)) % 5) as f64 - 2.0) * 8.0
        } else {
          warm
        };
        data.push((base + noise.next_offset(noise_amplitude / 3.0)).clamp(16.0, 240.0) as u8);
      }
    }
  }
  Ok(VideoFrame::new(resolution, timestamp_us, data)?)
}

/// A small pseudo-random generator (xorshift): sensor noise without a dependency.
struct Noise(u64);

impl Noise {
  /// Uniform in `-amplitude..amplitude`.
  fn next_offset(&mut self, amplitude: f64) -> f64 {
    self.0 ^= self.0 << 13;
    self.0 ^= self.0 >> 7;
    self.0 ^= self.0 << 17;
    ((self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) * amplitude
  }
}
