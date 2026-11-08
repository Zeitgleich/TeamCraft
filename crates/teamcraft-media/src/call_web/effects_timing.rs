//! Local timing of the browser's background effects (#1336), for comparing the WebGPU and the
//! WebGL2 compositing on a real device. Off unless asked for; nothing leaves the browser.
//!
//! - In the app: `localStorage.setItem("teamcraft.effectsTiming", "1")` (devtools console), then
//!   start a camera with an effect (a call or the pre-join check). Every 300 pictures (about 10 s)
//!   the call module logs a summary to the console (`effects timing`) and puts it on
//!   `globalThis.teamcraftEffectsTiming`. `localStorage.setItem("teamcraft.effectsCompositing",
//!   "webgl2")` (or `"webgpu"`) makes the browser composite that way where it can, to compare both
//!   in the same browser; `removeItem` returns to the default.
//! - The effects bench (`examples/effects_bench.rs`, `dev/effects-bench`) runs the same pipeline
//!   on a 720p test picture with a WebCodecs H.264 encoder and reports one summary for the run.
//!
//! The times are wall-clock milliseconds of the page's main thread around each step
//! (`performance.now()`); "composite" on WebGPU includes waiting for the GPU's read-back, which
//! leaves the main thread free meanwhile.

use std::time::Duration;

use web_time::Instant;

/// `localStorage` key that turns the timing on in the app ("1").
pub const TIMING_KEY: &str = "teamcraft.effectsTiming";
/// `localStorage` key that picks the compositing in the app: "webgpu" or "webgl2".
pub const COMPOSITING_KEY: &str = "teamcraft.effectsCompositing";
/// The global the app's summaries go to (the newest one).
const SUMMARY_GLOBAL: &str = "teamcraftEffectsTiming";
/// Pictures per summary in the app: about 10 s at 30 fps.
const PICTURES_PER_SUMMARY: usize = 300;

/// A step of compositing one picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
  /// The camera's picture onto the GPU: drawImage and getImageData (WebGPU), texImage2D (WebGL2).
  Picture,
  /// The model's 256x144 input, handed to the worker (only when a mask is due).
  ModelInput,
  /// Compositing: upload, passes and read-back as I420 (WebGPU), the GL passes (WebGL2).
  Composite,
  /// The `VideoFrame` for the encoders: from the I420 bytes (WebGPU), of the canvas (WebGL2).
  Frame,
  /// The whole picture, from the `<video>` to the `VideoFrame`.
  Total,
}

impl Stage {
  const ALL: [Stage; 5] = [
    Stage::Picture,
    Stage::ModelInput,
    Stage::Composite,
    Stage::Frame,
    Stage::Total,
  ];

  fn name(self) -> &'static str {
    match self {
      Stage::Picture => "picture",
      Stage::ModelInput => "model_input",
      Stage::Composite => "composite",
      Stage::Frame => "frame",
      Stage::Total => "total",
    }
  }
}

/// Per-picture times of the effects' steps, the worker's segmentation times and what the encoders
/// skipped, since the last summary.
#[derive(Debug)]
pub struct EffectsTiming {
  compositing: &'static str,
  /// Milliseconds per [`Stage`], one entry per picture that went through the step.
  samples: [Vec<f32>; 5],
  segmentation: Vec<f32>,
  /// Pictures the effects made a `VideoFrame` of.
  pictures: usize,
  /// Encoders' totals as last seen: frames given and frames skipped (queue full).
  encoded: u64,
  skipped: u64,
  /// The totals at the last summary.
  encoded_before: u64,
  skipped_before: u64,
  since: Instant,
  /// Summaries go to the console and the global every this many pictures; never for the bench.
  maybe_every: Option<usize>,
}

impl EffectsTiming {
  /// Timing for the app: on where `localStorage` says so ([`TIMING_KEY`]).
  pub fn from_storage(compositing: &'static str) -> Option<Self> {
    let is_on = web_sys::window()
      .and_then(|window| window.local_storage().ok().flatten())
      .and_then(|storage| storage.get_item(TIMING_KEY).ok().flatten())
      .is_some_and(|value| value == "1");
    is_on.then(|| Self::new(compositing, Some(PICTURES_PER_SUMMARY)))
  }

  /// Timing for one bench run: one summary at its end ([`EffectsTiming::summary`]).
  pub fn for_bench(compositing: &'static str) -> Self {
    Self::new(compositing, None)
  }

  fn new(compositing: &'static str, maybe_every: Option<usize>) -> Self {
    Self {
      compositing,
      samples: Default::default(),
      segmentation: Vec::new(),
      pictures: 0,
      encoded: 0,
      skipped: 0,
      encoded_before: 0,
      skipped_before: 0,
      since: Instant::now(),
      maybe_every,
    }
  }

  pub fn record(&mut self, stage: Stage, started: Instant) {
    let index = Stage::ALL.iter().position(|known| *known == stage).unwrap_or(0);
    self.samples[index].push(millis(started.elapsed()));
  }

  /// The worker answered with a mask after `cost` of its own time.
  pub fn record_segmentation(&mut self, cost: Duration) {
    self.segmentation.push(millis(cost));
  }

  /// A picture is done; in the app, every so many a summary goes out.
  pub fn picture_done(&mut self) {
    self.pictures += 1;
    if self.maybe_every.is_some_and(|every| self.pictures >= every) {
      let summary = self.summary();
      tracing::info!(target: "effects_timing", %summary, "effects timing");
      publish(&summary);
      self.restart();
    }
  }

  /// The encoders' totals of frames given and skipped because their queue was full (the encoder
  /// fell behind).
  pub fn observe_encoders(&mut self, encoded: u64, skipped: u64) {
    self.encoded = encoded;
    self.skipped = skipped;
  }

  fn restart(&mut self) {
    for samples in &mut self.samples {
      samples.clear();
    }
    self.segmentation.clear();
    self.pictures = 0;
    self.encoded_before = self.encoded;
    self.skipped_before = self.skipped;
    self.since = Instant::now();
  }

  /// The summary since the start or the last one, as JSON: per step the count, mean, p50, p95 and
  /// max in milliseconds, the worker's segmentations, and the encoders' frames.
  pub fn summary(&self) -> String {
    let stages = Stage::ALL
      .iter()
      .zip(&self.samples)
      .map(|(stage, samples)| format!("\"{}\":{}", stage.name(), statistics(samples)))
      .collect::<Vec<_>>()
      .join(",");
    format!(
      "{{\"compositing\":\"{}\",\"pictures\":{},\"seconds\":{:.2},\"stages\":{{{stages}}},\"segmentation\":{},\"encoder\":{{\"given\":{},\"skipped_queue_full\":{}}}}}",
      self.compositing,
      self.pictures,
      self.since.elapsed().as_secs_f64(),
      statistics(&self.segmentation),
      self.encoded.saturating_sub(self.encoded_before),
      self.skipped.saturating_sub(self.skipped_before),
    )
  }
}

fn millis(duration: Duration) -> f32 {
  duration.as_secs_f32() * 1000.0
}

/// `{"count":…,"mean":…,"p50":…,"p95":…,"max":…}` of `samples` (milliseconds).
fn statistics(samples: &[f32]) -> String {
  if samples.is_empty() {
    return "{\"count\":0}".to_owned();
  }
  let mut sorted = samples.to_vec();
  sorted.sort_by(f32::total_cmp);
  let at = |share: f32| sorted[((sorted.len() - 1) as f32 * share).round() as usize];
  let mean = sorted.iter().sum::<f32>() / sorted.len() as f32;
  format!(
    "{{\"count\":{},\"mean\":{mean:.2},\"p50\":{:.2},\"p95\":{:.2},\"max\":{:.2}}}",
    sorted.len(),
    at(0.5),
    at(0.95),
    sorted[sorted.len() - 1],
  )
}

/// The summary on `globalThis.teamcraftEffectsTiming`, as an object.
fn publish(summary: &str) {
  if let Ok(value) = js_sys::JSON::parse(summary) {
    let _ = js_sys::Reflect::set(&js_sys::global(), &SUMMARY_GLOBAL.into(), &value);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn statistics_of_no_samples_and_of_some() {
    assert_eq!(statistics(&[]), "{\"count\":0}");
    let samples: Vec<f32> = (1..=100).map(|n| n as f32).collect();
    assert_eq!(
      statistics(&samples),
      "{\"count\":100,\"mean\":50.50,\"p50\":51.00,\"p95\":95.00,\"max\":100.00}"
    );
  }

  #[test]
  fn a_summary_counts_the_encoders_since_the_last_one() {
    let mut timing = EffectsTiming::for_bench("webgl2");
    timing.observe_encoders(10, 2);
    timing.restart();
    timing.observe_encoders(25, 3);
    timing.record(Stage::Total, Instant::now());
    timing.picture_done();
    let summary = timing.summary();
    assert!(
      summary.starts_with("{\"compositing\":\"webgl2\",\"pictures\":1,"),
      "{summary}"
    );
    assert!(
      summary.contains("\"encoder\":{\"given\":15,\"skipped_queue_full\":1}"),
      "{summary}"
    );
    assert!(summary.contains("\"total\":{\"count\":1,"), "{summary}");
    assert!(summary.contains("\"picture\":{\"count\":0}"), "{summary}");
    assert!(is_balanced(&summary));
  }

  /// Balanced braces and quotes: enough to catch a format slip without a JSON parser here.
  fn is_balanced(json: &str) -> bool {
    let opens = json.matches('{').count();
    opens == json.matches('}').count() && json.matches('"').count().is_multiple_of(2)
  }
}
