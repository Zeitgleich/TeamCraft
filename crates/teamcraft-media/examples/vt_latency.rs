//! How long the hardware encoder takes per picture while a call's camera layers encode, with and
//! without load (issue #1719: on macOS, a picture later than the 50 ms wait is dropped and the
//! next frame forced to a keyframe).
//!
//! ```sh
//! cargo run --release -p teamcraft-media --features gstreamer,openh264-source \
//!   --example vt_latency -- [--calls 1] [--seconds 10] [--busy 0] [--noise 4] [--keyframe-every 0]
//! ```
//!
//! Each `--calls` encodes the camera layers of one 1080p camera (`video::camera_layers`, capped at
//! 720p like the default setting) at 30 fps, as the call's encoder thread does: every layer gets
//! the frame (`start`), then each picture is waited for (`finish`). `--busy` threads spin on the
//! CPU meanwhile. The report, logged as a table, counts, per layer, the frames that gave a picture, the frames that
//! gave none, the pictures that were keyframes, and the spread of `finish`'s wait. `--keyframe-every`
//! forces every so many frames to a keyframe, as a dropped late picture does.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use teamcraft_media::video::{VideoFrame, VideoResolution, camera_layers};
use teamcraft_media::video_codec::{VideoContent, VideoEncoder};
use teamcraft_media::video_gstreamer::GstreamerEncoder;
use tracing::info;

const FRAMERATE: u64 = 30;
const SOURCE: VideoResolution = VideoResolution {
  width: 1920,
  height: 1080,
};
const LIMIT: VideoResolution = VideoResolution {
  width: 1280,
  height: 720,
};

#[derive(Default, Clone)]
struct Tally {
  pictures: u64,
  missing: u64,
  keyframes: u64,
  waits_us: Vec<u64>,
}

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt().with_target(false).init();
  let mut calls = 1usize;
  let mut seconds = 10u64;
  let mut busy = 0usize;
  let mut noise = 4u8;
  let mut keyframe_every = 0u64;
  let mut args = std::env::args().skip(1);
  while let Some(flag) = args.next() {
    let value = args.next().context("every flag takes a value")?;
    match flag.as_str() {
      "--calls" => calls = value.parse()?,
      "--seconds" => seconds = value.parse()?,
      "--busy" => busy = value.parse()?,
      "--noise" => noise = value.parse()?,
      "--keyframe-every" => keyframe_every = value.parse()?,
      other => bail!("unknown flag {other}"),
    }
  }
  let stop = Arc::new(AtomicBool::new(false));
  let spinners: Vec<_> = (0..busy)
    .map(|_| {
      let stop = stop.clone();
      std::thread::spawn(move || {
        let mut x = 0u64;
        while !stop.load(Ordering::Relaxed) {
          x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
          std::hint::black_box(x);
        }
      })
    })
    .collect();
  let workers: Vec<_> = (0..calls)
    .map(|call| std::thread::spawn(move || run_call(call, seconds, noise, keyframe_every)))
    .collect();
  let mut totals: Vec<Tally> = Vec::new();
  for worker in workers {
    let tallies = worker
      .join()
      .map_err(|_| anyhow::anyhow!("a call thread panicked"))??;
    if totals.is_empty() {
      totals = vec![Tally::default(); tallies.len()];
    }
    for (total, tally) in totals.iter_mut().zip(tallies) {
      total.pictures += tally.pictures;
      total.missing += tally.missing;
      total.keyframes += tally.keyframes;
      total.waits_us.extend(tally.waits_us);
    }
  }
  stop.store(true, Ordering::Relaxed);
  for spinner in spinners {
    let _ = spinner.join();
  }
  let layers = camera_layers(SOURCE, FRAMERATE as u8, LIMIT);
  info!("calls={calls} busy={busy} seconds={seconds} noise={noise} keyframe_every={keyframe_every}");
  info!("layer      pictures missing keyframes  wait p50   p90   p99   max (ms)");
  for (layer, mut tally) in layers.iter().zip(totals) {
    tally.waits_us.sort_unstable();
    let at = |share: f64| {
      let index = ((tally.waits_us.len() as f64 - 1.0) * share).round() as usize;
      tally.waits_us.get(index).copied().unwrap_or(0) as f64 / 1000.0
    };
    info!(
      "{:>4}x{:<4} {:>9} {:>7} {:>9} {:>9.1} {:>5.1} {:>5.1} {:>5.1}",
      layer.width,
      layer.height,
      tally.pictures,
      tally.missing,
      tally.keyframes,
      at(0.5),
      at(0.9),
      at(0.99),
      at(1.0)
    );
  }
  Ok(())
}

fn run_call(call: usize, seconds: u64, noise: u8, keyframe_every: u64) -> anyhow::Result<Vec<Tally>> {
  let layers = camera_layers(SOURCE, FRAMERATE as u8, LIMIT);
  let mut encoders: Vec<GstreamerEncoder> = layers
    .iter()
    .map(|layer| GstreamerEncoder::new(layer, VideoContent::Camera).context("no hardware encoder"))
    .collect::<anyhow::Result<_>>()?;
  let mut tallies = vec![Tally::default(); layers.len()];
  let interval = Duration::from_micros(1_000_000 / FRAMERATE);
  let started = Instant::now();
  let mut seed = 0x9e37_79b9_u32.wrapping_add(call as u32);
  for index in 0..seconds * FRAMERATE {
    let due = started + interval * index as u32;
    if let Some(wait) = due.checked_duration_since(Instant::now()) {
      std::thread::sleep(wait);
    }
    let timestamp_us = index * 1_000_000 / FRAMERATE;
    let frame = noisy_frame(index, timestamp_us, noise, &mut seed)?;
    let is_first = index == 0 || (keyframe_every > 0 && index % keyframe_every == 0);
    for encoder in &mut encoders {
      encoder.start(&frame, is_first)?;
    }
    for (encoder, tally) in encoders.iter_mut().zip(&mut tallies) {
      let before = Instant::now();
      let picture = encoder.finish(&frame, is_first)?;
      // The first second: sessions starting.
      if index < FRAMERATE {
        continue;
      }
      tally.waits_us.push(before.elapsed().as_micros() as u64);
      match picture {
        Some(picture) => {
          tally.pictures += 1;
          tally.keyframes += u64::from(picture.is_keyframe);
        },
        None => tally.missing += 1,
      }
    }
  }
  Ok(tallies)
}

/// A slowly moving gradient with sensor noise of up to `noise` per pixel, in NV12.
fn noisy_frame(index: u64, timestamp_us: u64, noise: u8, seed: &mut u32) -> anyhow::Result<VideoFrame> {
  let (width, height) = (usize::from(SOURCE.width), usize::from(SOURCE.height));
  let mut data = Vec::with_capacity(width * height * 3 / 2);
  let shift = index as usize;
  for y in 0..height {
    for x in 0..width {
      *seed ^= *seed << 13;
      *seed ^= *seed >> 17;
      *seed ^= *seed << 5;
      let jitter = if noise == 0 {
        0
      } else {
        (*seed % (2 * u32::from(noise) + 1)) as i32 - i32::from(noise)
      };
      let base = (((x + shift) / 8 + y / 8) % 200) as i32 + 30;
      data.push((base + jitter).clamp(16, 235) as u8);
    }
  }
  for y in 0..height / 2 {
    for x in 0..width / 2 {
      data.push(((x + y + shift) % 64 + 96) as u8);
      data.push(((x * 2 + shift) % 64 + 96) as u8);
    }
  }
  Ok(VideoFrame::new(SOURCE, timestamp_us, data)?)
}
