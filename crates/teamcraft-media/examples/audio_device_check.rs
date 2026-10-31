//! Lists the microphones and speakers the call engine sees and checks each one for real: every
//! microphone is captured for a few seconds (level and sample rate), and every speaker plays a
//! tone that is captured back from its monitor (Linux, PipeWire: the sink's monitor is opened as
//! an input by the same id). Issue #1072.
//!
//! ```text
//! cargo run -p teamcraft-media --features pipewire --example audio_device_check
//! ```
//!
//! Exits non-zero when a device fails to open, delivers nothing, a speaker's tone doesn't come
//! back, or the device list reports a change meanwhile (nothing was plugged in or out).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::bail;
use teamcraft_media::audio_device::{
  AudioInput, AudioOutput, PlaybackBuffer, audio_backend, device_changes, list_input_devices,
  list_output_devices,
};
use teamcraft_media::audio_frame::SAMPLE_RATE_HZ;
use teamcraft_media::audio_io::{AudioDeviceChoice, AudioDeviceInfo, sine_tone};
use tracing::{info, warn};

const CAPTURE_TIME: Duration = Duration::from_secs(3);
const TONE_HZ: f32 = 1000.0;
const TONE_AMPLITUDE: f32 = 0.25;
/// A captured tone counts as back above this level (the tone itself is about -15 dBFS RMS).
const MIN_LOOPBACK_DBFS: f32 = -40.0;
/// Captured samples per second may differ this much from 48 kHz (start-up, scheduling).
const RATE_TOLERANCE: f32 = 0.1;

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt()
    .with_target(false)
    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?))
    .init();
  info!(backend = ?audio_backend(), "checking the audio devices");
  let inputs = list_input_devices()?;
  let outputs = list_output_devices()?;
  log_list("input", &inputs);
  log_list("output", &outputs);
  // #1072: opening and closing devices must not look like devices coming and going.
  let maybe_changes = device_changes();
  let mut failures = 0;
  for device in &inputs {
    let choice = AudioDeviceChoice::Device(device.id.clone());
    match capture(&choice, None) {
      Ok(measured) => {
        let is_ok = measured.peak > 0.0 && measured.has_rate();
        info!(
          device = %device.name,
          rms_dbfs = format!("{:.1}", measured.rms_dbfs()),
          peak_dbfs = format!("{:.1}", dbfs(measured.peak)),
          samples_per_second = format!("{:.0}", measured.rate_hz()),
          is_ok,
          "microphone"
        );
        failures += usize::from(!is_ok);
      },
      Err(error) => {
        warn!(device = %device.name, %error, "microphone failed");
        failures += 1;
      },
    }
  }
  for device in &outputs {
    let choice = AudioDeviceChoice::Device(device.id.clone());
    match loopback(&choice) {
      Ok(measured) => {
        let is_ok = measured.rms_dbfs() > MIN_LOOPBACK_DBFS && measured.has_rate();
        info!(
          device = %device.name,
          tone_back_dbfs = format!("{:.1}", measured.rms_dbfs()),
          samples_per_second = format!("{:.0}", measured.rate_hz()),
          is_ok,
          "speakers (tone captured from the monitor)"
        );
        failures += usize::from(!is_ok);
      },
      Err(error) => {
        warn!(device = %device.name, %error, "speakers failed");
        failures += 1;
      },
    }
  }
  match maybe_changes {
    Some(changes) => {
      let is_ok = !changes.has_changed().unwrap_or(true);
      info!(
        is_ok,
        "device list unchanged while the devices were opened and closed"
      );
      failures += usize::from(!is_ok);
    },
    None => info!("this backend has no device events; not checked"),
  }
  if failures > 0 {
    bail!("{failures} device check(s) failed");
  }
  Ok(())
}

/// What a capture delivered.
#[derive(Debug, Default)]
struct Measured {
  samples: usize,
  sum_squares: f64,
  peak: f32,
  elapsed: Duration,
}

impl Measured {
  fn rms_dbfs(&self) -> f32 {
    dbfs((self.sum_squares / self.samples.max(1) as f64).sqrt() as f32)
  }

  fn rate_hz(&self) -> f32 {
    self.samples as f32 / self.elapsed.as_secs_f32()
  }

  fn has_rate(&self) -> bool {
    (self.rate_hz() / SAMPLE_RATE_HZ as f32 - 1.0).abs() < RATE_TOLERANCE
  }
}

/// Captures `choice` for [`CAPTURE_TIME`], skipping `maybe_skip` at the start (a tone that is
/// still starting).
fn capture(choice: &AudioDeviceChoice, maybe_skip: Option<Duration>) -> anyhow::Result<Measured> {
  let measured = Arc::new(Mutex::new(Measured::default()));
  let skip_samples = maybe_skip.map_or(0, |skip| (skip.as_secs_f32() * SAMPLE_RATE_HZ as f32) as usize);
  let mut seen = 0;
  let sink = Arc::clone(&measured);
  let input = AudioInput::open(choice, move |samples| {
    seen += samples.len();
    if seen <= skip_samples {
      return;
    }
    let mut measured = sink.lock().unwrap_or_else(PoisonError::into_inner);
    measured.samples += samples.len();
    for sample in samples {
      measured.sum_squares += f64::from(sample * sample);
      measured.peak = measured.peak.max(sample.abs());
    }
  })?;
  let started = Instant::now();
  std::thread::sleep(CAPTURE_TIME + maybe_skip.unwrap_or_default());
  drop(input);
  let mut measured = std::mem::take(&mut *measured.lock().unwrap_or_else(PoisonError::into_inner));
  measured.elapsed = started.elapsed() - maybe_skip.unwrap_or_default();
  Ok(measured)
}

/// Plays a tone on `choice` and captures it back from the device's monitor.
fn loopback(choice: &AudioDeviceChoice) -> anyhow::Result<Measured> {
  let seconds = CAPTURE_TIME + Duration::from_secs(2);
  let buffer = Arc::new(PlaybackBuffer::new(SAMPLE_RATE_HZ as usize * 10));
  let tone = sine_tone(
    TONE_HZ,
    TONE_AMPLITUDE,
    0,
    (seconds.as_secs_f32() * SAMPLE_RATE_HZ as f32) as usize,
  );
  buffer.push(&tone);
  let _output = AudioOutput::open(choice, buffer)?;
  capture(choice, Some(Duration::from_millis(500)))
}

fn log_list(direction: &str, devices: &[AudioDeviceInfo]) {
  for device in devices {
    info!(direction, id = %device.id, name = %device.name, is_default = device.is_default, "device");
  }
}

fn dbfs(level: f32) -> f32 {
  20.0 * level.max(1e-9).log10()
}
