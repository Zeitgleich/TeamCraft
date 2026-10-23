//! The ringtone of an incoming call (notifications.md, "Calls"): a warm electric-piano arpeggio
//! (F4, A4, C5, E5, then C5 and E5 again) with a soft echo, then a pause; one pass lasts three
//! seconds and repeats while the call rings. Synthesized (two-operator FM, the classic electric
//! piano recipe), so there is no sound file and nothing to license. [`RingtonePlayer`] plays it on
//! an output device until it is dropped.

use crate::audio_frame::SAMPLE_RATE_HZ;

/// Peak level of the whole pass: about −8 dBFS, noticeable across a room without being harsh.
const PEAK: f32 = 0.4;
/// One pass, echo included, then silence.
const PASS_SECONDS: f32 = 3.0;
/// The arpeggio: (frequency, start in seconds). Each note sounds a little softer than the last.
const NOTES: &[(f32, f32)] = &[
  (349.23, 0.0),
  (440.0, 0.11),
  (523.25, 0.22),
  (659.25, 0.33),
  (523.25, 0.55),
  (659.25, 0.66),
];
const NOTE_SECONDS: f32 = 0.9;
/// How much softer each further note is.
const VELOCITY_STEP: f32 = 0.06;
/// FM modulation depth at the attack (brightness); it fades fast, so notes start bright and mellow.
const MODULATION_INDEX: f32 = 1.6;
const MODULATION_DECAY_PER_SECOND: f32 = 6.0;
const DECAY_PER_SECOND: f32 = 3.2;
/// Attack time constant and release ramp: no clicks.
const ATTACK_SECONDS: f32 = 0.003;
const RELEASE_SECONDS: f32 = 0.05;
/// The second harmonic's share: a little body under the FM tone.
const OVERTONE: f32 = 0.15;
/// The echo: delay, feedback and wet share.
const ECHO_SECONDS: f32 = 0.22;
const ECHO_FEEDBACK: f32 = 0.35;
const ECHO_MIX: f32 = 0.4;

/// The samples of one pass (mono, 48 kHz); play it in a loop.
pub fn synthesize() -> Vec<f32> {
  let length = seconds_to_samples(PASS_SECONDS);
  let mut dry = vec![0.0f32; length];
  for (index, (frequency_hz, start)) in NOTES.iter().enumerate() {
    let velocity = 1.0 - VELOCITY_STEP * index as f32;
    let offset = seconds_to_samples(*start);
    for (position, sample) in electric_piano(*frequency_hz, velocity).into_iter().enumerate() {
      if let Some(slot) = dry.get_mut(offset + position) {
        *slot += sample;
      }
    }
  }
  let mut samples = with_echo(&dry);
  let peak = samples.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
  if peak > 0.0 {
    samples.iter_mut().for_each(|sample| *sample *= PEAK / peak);
  }
  samples
}

/// One note: a sine carrier phase-modulated by a sine at the same frequency, with a fading index.
fn electric_piano(frequency_hz: f32, velocity: f32) -> Vec<f32> {
  let count = seconds_to_samples(NOTE_SECONDS);
  (0..count)
    .map(|index| {
      let time = index as f32 / SAMPLE_RATE_HZ as f32;
      let phase = std::f32::consts::TAU * frequency_hz * time;
      let modulation = MODULATION_INDEX * velocity * (-MODULATION_DECAY_PER_SECOND * time).exp();
      let envelope = (1.0 - (-time / ATTACK_SECONDS).exp())
        * (-DECAY_PER_SECOND * time).exp()
        * ((NOTE_SECONDS - time) / RELEASE_SECONDS).min(1.0);
      envelope * ((phase + modulation * phase.sin()).sin() + OVERTONE * (2.0 * phase).sin())
    })
    .collect()
}

/// A feedback echo mixed under the dry signal; the pass is long enough for it to die away.
fn with_echo(dry: &[f32]) -> Vec<f32> {
  let delay = seconds_to_samples(ECHO_SECONDS);
  let mut wet = dry.to_vec();
  for index in delay..wet.len() {
    wet[index] += ECHO_FEEDBACK * wet[index - delay];
  }
  dry
    .iter()
    .zip(&wet)
    .map(|(dry, wet)| (1.0 - ECHO_MIX) * dry + ECHO_MIX * wet)
    .collect()
}

fn seconds_to_samples(seconds: f32) -> usize {
  (seconds * SAMPLE_RATE_HZ as f32).round() as usize
}

#[cfg(feature = "native")]
pub use player::RingtonePlayer;

#[cfg(feature = "native")]
mod player {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::thread::JoinHandle;
  use std::time::Duration;

  use tracing::warn;

  use crate::audio_device::{AudioOutput, PlaybackBuffer};
  use crate::audio_frame::SAMPLE_RATE_HZ;
  use crate::audio_io::AudioDeviceChoice;

  /// How much is queued ahead of the speakers; also how late stopping can be heard.
  const AHEAD_SECONDS: f32 = 0.2;
  /// How often the feeding thread tops the queue up.
  const FEED_INTERVAL: Duration = Duration::from_millis(40);

  /// Plays the ringtone in a loop on its own thread (the audio stream may not move between threads
  /// on every platform) until dropped.
  pub struct RingtonePlayer {
    is_stopped: Arc<AtomicBool>,
    maybe_thread: Option<JoinHandle<()>>,
  }

  impl RingtonePlayer {
    /// Starts ringing on `device`. A missing or failing device is logged; the call still shows.
    pub fn start(device: AudioDeviceChoice) -> Self {
      let is_stopped = Arc::new(AtomicBool::new(false));
      let stop = is_stopped.clone();
      let spawned = std::thread::Builder::new()
        .name("ringtone".to_owned())
        .spawn(move || ring(&device, &stop));
      let maybe_thread = spawned
        .inspect_err(|error| warn!(%error, "could not start the ringtone"))
        .ok();
      Self {
        is_stopped,
        maybe_thread,
      }
    }
  }

  impl Drop for RingtonePlayer {
    fn drop(&mut self) {
      self.is_stopped.store(true, Ordering::Relaxed);
      if let Some(thread) = self.maybe_thread.take()
        && thread.join().is_err()
      {
        warn!("the ringtone thread panicked");
      }
    }
  }

  fn ring(device: &AudioDeviceChoice, is_stopped: &AtomicBool) {
    let ahead = (AHEAD_SECONDS * SAMPLE_RATE_HZ as f32) as usize;
    let buffer = Arc::new(PlaybackBuffer::new(ahead * 2));
    let _output = match AudioOutput::open(device, buffer.clone()) {
      Ok(output) => output,
      Err(error) => {
        warn!(%error, "no speakers for the ringtone");
        return;
      },
    };
    let pass = super::synthesize();
    let mut position = 0;
    while !is_stopped.load(Ordering::Relaxed) {
      while buffer.len() < ahead {
        let count = (ahead - buffer.len()).min(pass.len() - position);
        buffer.push(&pass[position..position + count]);
        position = (position + count) % pass.len();
      }
      std::thread::sleep(FEED_INTERVAL);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_ringtone_is_audible_but_not_harsh_and_never_clicks() {
    let samples = synthesize();
    let peak = samples.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    assert!((peak - PEAK).abs() < 1e-4, "peaks at {peak}");
    assert!(samples[0].abs() < 1e-3 && samples[samples.len() - 1].abs() < 1e-3);
    let largest_step = samples
      .windows(2)
      .map(|pair| (pair[1] - pair[0]).abs())
      .fold(0.0f32, f32::max);
    assert!(largest_step < 0.1, "jumps by {largest_step}");
  }

  #[test]
  fn one_pass_lasts_three_seconds_and_ends_quietly_before_the_next() {
    let samples = synthesize();
    assert_eq!(samples.len(), seconds_to_samples(PASS_SECONDS));
    let last_half_second = &samples[samples.len() - seconds_to_samples(0.5)..];
    assert!(last_half_second.iter().all(|sample| sample.abs() < 0.01));
  }
}
