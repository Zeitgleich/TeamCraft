//! What a screen share costs and sends on the desktop, per profile (issue #565): the layers of a
//! standard or 4K share in detail or motion mode (`video::ScreenProfile`), encoded as the call's
//! encoder thread does, in real time, from a synthetic screen of text.
//!
//! ```sh
//! cargo run --release -p teamcraft-media --features gstreamer,openh264-source \
//!   --example screen_bench -- [--profile detail|motion|detail-4k|motion-4k] \
//!   [--source 3840x2160] [--scene still|scroll] [--seconds 10] [--encoder preferred|software] \
//!   [--frames every|changes]
//! ```
//!
//! `--source` is the display's size in pixels (default 3840x2160, a 4K display); the profile
//! shrinks it to its limits. `--scene still` is a page of text where one line is typed (a
//! character every few frames, a blinking cursor), `scroll` the page scrolling steadily. With
//! `--frames every` (the default) every frame is encoded, at the profile's frame rate: the worst
//! case. `--frames changes` encodes only frames that differ from the last one, plus the last one
//! again after a second without change, as a share does (the capture delivers changes only, and
//! the encoder thread refreshes a still screen, `video_pipeline::SCREEN_IDLE_REFRESH`). `--encoder preferred` (the default) encodes as the call does
//! (VideoToolbox, VA-API), `software` with OpenH264.
//!
//! The report gives the encoding's cost per frame (the process's CPU time over all threads, the
//! hardware encoder's included, and the wall time), each layer's bitrate against its target and
//! the luma PSNR of a sample of its pictures, decoded with OpenH264. The numbers of #565 are in
//! docs/performance.md ("Screen share: profiles and codecs").

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use teamcraft_media::video::{ScreenProfile, VideoFrame, VideoResolution};
use teamcraft_media::video_codec::{OpenH264Decoder, SimulcastEncoder, VideoContent, VideoDecoder};
use teamcraft_media::video_pipeline::{
  EncoderFactory, SCREEN_IDLE_REFRESH, preferred_encoders, software_encoders,
};
use teamcraft_proto::{ScreenContentMode, ScreenShareResolution, VideoLayerInfo};
use tracing::info;

/// Left out of the rates and costs: the first keyframes and the rate control settling.
const WARM_UP: Duration = Duration::from_secs(1);
/// Every this many pictures of a layer is compared with its source (PSNR).
const PSNR_EVERY: u64 = 10;
/// An encoder may give a frame's picture a frame or two later: the best match of the last few
/// sources counts.
const RECENT_SOURCES: usize = 3;
/// Luma of the page and of the text.
const PAPER: u8 = 235;
const INK: u8 = 40;

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt().with_target(false).init();
  let mut arguments = std::env::args().skip(1);
  let mut profile = ScreenProfile::default();
  let mut source = VideoResolution {
    width: 3840,
    height: 2160,
  };
  let mut is_scrolling = false;
  let mut seconds = 10u64;
  let mut encoder = "preferred".to_owned();
  let mut is_changes_only = false;
  while let Some(argument) = arguments.next() {
    match (argument.as_str(), arguments.next()) {
      ("--profile", Some(value)) => profile = parse_profile(&value)?,
      ("--source", Some(value)) => source = parse_size(&value)?,
      ("--scene", Some(value)) => is_scrolling = parse_scene(&value)?,
      ("--seconds", Some(value)) => seconds = value.parse().context("--seconds takes a number")?,
      ("--encoder", Some(value)) => encoder = value,
      ("--frames", Some(value)) => is_changes_only = parse_frames(&value)?,
      (other, _) => bail!("unknown argument {other}"),
    }
  }
  let encoders: EncoderFactory = match encoder.as_str() {
    "preferred" => preferred_encoders(),
    "software" => software_encoders(),
    other => bail!("unknown encoder {other}; use preferred or software"),
  };
  let (limit, framerate) = profile.limits();
  let layers = profile.layers(source, framerate);
  let top = layers.last().context("no layers")?;
  let size = VideoResolution {
    width: top.width,
    height: top.height,
  };
  info!(?profile, ?limit, framerate, "profile");
  let page = Page::new(size);
  let mut encoding = Encoding::new(layers, &encoders)?;
  let interval = Duration::from_secs(1) / u32::from(framerate);
  let duration = Duration::from_secs(seconds);
  let mut index = 0u64;
  let mut maybe_last: Option<(VideoFrame, Instant)> = None;
  while encoding.started.elapsed() < duration {
    let due = encoding.started + interval * u32::try_from(index)?;
    if let Some(wait) = due.checked_duration_since(Instant::now()) {
      std::thread::sleep(wait);
    }
    // When the frame was due, as a capture stamps it, also when the encoding runs late.
    let timestamp_us = u64::try_from((interval * u32::try_from(index)?).as_micros())?;
    let frame = page.frame(index, is_scrolling, timestamp_us)?;
    index += 1;
    let is_unchanged = maybe_last.as_ref().is_some_and(|(last, encoded_at)| {
      last.data() == frame.data() && encoded_at.elapsed() < SCREEN_IDLE_REFRESH
    });
    if is_changes_only && is_unchanged {
      continue;
    }
    encoding.encode(&frame)?;
    maybe_last = Some((frame, Instant::now()));
  }
  report(&encoding, framerate);
  Ok(())
}

fn parse_profile(value: &str) -> anyhow::Result<ScreenProfile> {
  let (mode, size) = match value {
    "detail" => (ScreenContentMode::Detail, ScreenShareResolution::Standard),
    "motion" => (ScreenContentMode::Motion, ScreenShareResolution::Standard),
    "detail-4k" => (ScreenContentMode::Detail, ScreenShareResolution::Uhd),
    "motion-4k" => (ScreenContentMode::Motion, ScreenShareResolution::Uhd),
    other => bail!("unknown profile {other}; use detail, motion, detail-4k or motion-4k"),
  };
  Ok(ScreenProfile { mode, size })
}

fn parse_scene(value: &str) -> anyhow::Result<bool> {
  match value {
    "still" => Ok(false),
    "scroll" => Ok(true),
    other => bail!("unknown scene {other}; use still or scroll"),
  }
}

fn parse_frames(value: &str) -> anyhow::Result<bool> {
  match value {
    "every" => Ok(false),
    "changes" => Ok(true),
    other => bail!("unknown frames {other}; use every or changes"),
  }
}

fn parse_size(value: &str) -> anyhow::Result<VideoResolution> {
  let (width, height) = value.split_once('x').context("--source takes WIDTHxHEIGHT")?;
  Ok(VideoResolution {
    width: width.parse()?,
    height: height.parse()?,
  })
}

/// A page of text three screens tall, as luma rows: lines of words (dark runs of glyph-like
/// strokes) on paper, at the line height of a HiDPI display's text at its size.
struct Page {
  size: VideoResolution,
  rows: Vec<Vec<u8>>,
  line_height: usize,
}

impl Page {
  fn new(size: VideoResolution) -> Self {
    let (width, height) = (usize::from(size.width), usize::from(size.height));
    // 19 px lines at 1080 rows, 38 at 2160: the same text, more pixels.
    let line_height = (height * 19 / 1080).max(8);
    let glyph = (line_height * 7 / 19).max(3);
    let mut random = Random(0x2545_f491_4f6c_dd1d);
    let mut rows = vec![vec![PAPER; width]; height * 3];
    for (line, lines) in rows.chunks_mut(line_height).enumerate() {
      let indent = (random.next() % 6) as usize * glyph * 2;
      let mut x = width / 8 + indent + line % 3;
      let end = width.saturating_sub(width / 16);
      while x + glyph < end {
        let word = 2 + (random.next() % 9) as usize;
        for _ in 0..word {
          // A glyph like a pixel font's: some of seven strokes (two upright on each side, three
          // across), as thick as a HiDPI display draws them.
          let strokes = random.next() | 1;
          let (top, glyph_height) = (line_height / 5, line_height * 3 / 5);
          let thickness = (glyph / 5).max(1);
          let ink = |row: usize, column: usize| {
            let (is_left, is_right) = (column < thickness, column + thickness >= glyph - 1);
            let is_upper = row < glyph_height / 2;
            let across = |at: usize| row + thickness / 2 >= at && row < at + thickness.div_ceil(2) + 1;
            (strokes & 1 != 0 && across(0))
              || (strokes & 2 != 0 && across(glyph_height / 2))
              || (strokes & 4 != 0 && across(glyph_height - thickness))
              || (strokes & 8 != 0 && is_left && is_upper)
              || (strokes & 16 != 0 && is_left && !is_upper)
              || (strokes & 32 != 0 && is_right && is_upper)
              || (strokes & 64 != 0 && is_right && !is_upper)
          };
          for (row, cells) in lines.iter_mut().skip(top).take(glyph_height).enumerate() {
            for column in 0..glyph.saturating_sub(1) {
              if ink(row, column) && x + column < width {
                cells[x + column] = INK;
              }
            }
          }
          x += glyph;
        }
        x += glyph;
      }
    }
    Self {
      size,
      rows,
      line_height,
    }
  }

  /// The screen at frame `index`: the page scrolled by a line every half second, or still with a
  /// line being typed.
  fn frame(&self, index: u64, is_scrolling: bool, timestamp_us: u64) -> anyhow::Result<VideoFrame> {
    let (width, height) = (usize::from(self.size.width), usize::from(self.size.height));
    let offset = if is_scrolling {
      (index as usize * self.line_height / 15) % (self.rows.len() - height)
    } else {
      0
    };
    let mut data = Vec::with_capacity(width * height * 3 / 2);
    for row in &self.rows[offset..offset + height] {
      data.extend_from_slice(row);
    }
    if !is_scrolling {
      // A line being typed: a character every fourth frame, a cursor blinking twice a second.
      let typed = (index / 4 % 80) as usize;
      let glyph = (self.line_height * 7 / 19).max(3);
      let top = self.line_height * 12;
      for row in top..top + self.line_height {
        let line = &mut data[row * width..(row + 1) * width];
        line[width / 8..].fill(PAPER);
        for column in 0..typed * glyph {
          if (row + column) % 3 != 0 && width / 8 + column < width {
            line[width / 8 + column] = INK;
          }
        }
        if (index / 15).is_multiple_of(2) && width / 8 + typed * glyph + 2 < width {
          line[width / 8 + typed * glyph + 1] = INK;
        }
      }
    }
    data.resize(width * height * 3 / 2, 128);
    Ok(VideoFrame::new(self.size, timestamp_us, data)?)
  }
}

/// A small pseudo-random generator (xorshift).
struct Random(u64);

impl Random {
  fn next(&mut self) -> u64 {
    self.0 ^= self.0 << 13;
    self.0 ^= self.0 >> 7;
    self.0 ^= self.0 << 17;
    self.0
  }
}

/// What one layer sent.
#[derive(Debug, Default)]
struct LayerTally {
  steady_pictures: u64,
  steady_bytes: u64,
  pictures: u64,
  keyframes: u64,
  psnr_sum: f64,
  psnr_samples: u64,
}

/// The share's encoders as the call runs them, and what they cost and sent.
struct Encoding {
  layers: Vec<VideoLayerInfo>,
  simulcast: SimulcastEncoder,
  decoders: BTreeMap<u8, OpenH264Decoder>,
  tallies: BTreeMap<u8, LayerTally>,
  recent: VecDeque<VideoFrame>,
  started: Instant,
  frames: u32,
  cpu: Duration,
  wall: Duration,
}

impl Encoding {
  fn new(layers: Vec<VideoLayerInfo>, encoders: &EncoderFactory) -> anyhow::Result<Self> {
    let simulcast = SimulcastEncoder::new(&layers, |layer| encoders(layer, VideoContent::Screen))?;
    let decoders = layers
      .iter()
      .map(|layer| Ok((layer.layer, OpenH264Decoder::new()?)))
      .collect::<anyhow::Result<_>>()?;
    Ok(Self {
      layers,
      simulcast,
      decoders,
      tallies: BTreeMap::new(),
      recent: VecDeque::new(),
      started: Instant::now(),
      frames: 0,
      cpu: Duration::ZERO,
      wall: Duration::ZERO,
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
      self.frames += 1;
      self.cpu += cpu_time().saturating_sub(cpu_before);
      self.wall += wall_before.elapsed();
    }
    for picture in pictures {
      let tally = self.tallies.entry(picture.layer.value()).or_default();
      tally.pictures += 1;
      tally.keyframes += u64::from(picture.is_keyframe);
      if is_steady {
        tally.steady_pictures += 1;
        tally.steady_bytes += picture.data.len() as u64;
      }
      let maybe_decoded = match self.decoders.get_mut(&picture.layer.value()) {
        Some(decoder) => decoder.decode(&picture.data, frame.timestamp_us())?,
        None => None,
      };
      if let Some(decoded) = maybe_decoded.filter(|_| is_steady && tally.pictures.is_multiple_of(PSNR_EVERY))
      {
        let mut best = 0.0f64;
        for source in &self.recent {
          best = best.max(luma_psnr(&source.scaled(decoded.resolution())?, &decoded));
        }
        tally.psnr_sum += best;
        tally.psnr_samples += 1;
      }
    }
    Ok(())
  }
}

fn report(encoding: &Encoding, framerate: u8) {
  let frames = f64::from(encoding.frames.max(1));
  let per_frame = |total: Duration| total.as_secs_f64() * 1000.0 / frames;
  let budget_ms = 1000.0 / f64::from(framerate.max(1));
  info!(
    "encoder: {}; {:.2} ms CPU ({:.0} % of a core), {:.2} ms wall per frame",
    encoding.simulcast.encoder_names(),
    per_frame(encoding.cpu),
    per_frame(encoding.cpu) / budget_ms * 100.0,
    per_frame(encoding.wall),
  );
  let steady_seconds = encoding
    .started
    .elapsed()
    .saturating_sub(WARM_UP)
    .as_secs_f64()
    .max(0.001);
  info!(
    "{:<6} {:>10} {:>12} {:>12} {:>6} {:>10} {:>8}",
    "layer", "size", "target", "sent", "fps", "keyframes", "PSNR"
  );
  for layer in &encoding.layers {
    let Some(tally) = encoding.tallies.get(&layer.layer) else {
      continue;
    };
    let psnr = if tally.psnr_samples > 0 {
      format!("{:.1} dB", tally.psnr_sum / tally.psnr_samples as f64)
    } else {
      "-".to_owned()
    };
    info!(
      "{:<6} {:>10} {:>7.0} kb/s {:>7.0} kb/s {:>6.1} {:>10} {:>8}",
      layer.layer,
      format!("{}x{}", layer.width, layer.height),
      f64::from(layer.max_bitrate_bps) / 1000.0,
      tally.steady_bytes as f64 * 8.0 / steady_seconds / 1000.0,
      tally.steady_pictures as f64 / steady_seconds,
      tally.keyframes,
      psnr,
    );
  }
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
