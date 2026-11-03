//! The app's own sounds for new messages (notifications.md, "Message sounds"; #800). Short and
//! synthesized, like the ringtone ([`crate::ringtone`]): there are no sound files and nothing to
//! license. The "uh-oh" is our own as well: two vowels sung by additive formant synthesis, a
//! falling interval in the spirit of the early messengers, not a recording of anyone.
//!
//! [`synthesize`] renders one (mono, 48 kHz) to play on the desktop's speakers or through Web
//! Audio; [`cut_to`] fits a user's own sound file to its length limit. Pure: rendering is
//! deterministic and the callers cache the result, so a sound is made once per run.

use std::f32::consts::TAU;
use std::time::Duration;

use teamcraft_client_api::message_sound::BuiltInSound;

use crate::audio_frame::SAMPLE_RATE_HZ;
use crate::ringtone::electric_piano;

/// Peak level of every sound: about −10 dBFS, a notice, a little softer than the ringtone.
const PEAK: f32 = 0.3;
/// Attack time constant of struck notes: fast, but without a click.
const ATTACK_SECONDS: f32 = 0.002;
/// Every sound ends with this fade, so nothing clicks when it stops.
const TAIL_FADE_SECONDS: f32 = 0.03;
/// A user's sound cut at its limit fades out over this.
const CUT_FADE_SECONDS: f32 = 0.05;

/// A bell: the strike tone and the inharmonic partials that make it ring.
const BELL: &[Partial] = &[
  Partial::new(1.0, 1.0, 4.5),
  Partial::new(2.0, 0.3, 7.0),
  Partial::new(2.76, 0.22, 10.0),
  Partial::new(5.4, 0.08, 16.0),
];
/// A small, bright bell that dies away quickly.
const PING_BELL: &[Partial] = &[
  Partial::new(1.0, 1.0, 8.0),
  Partial::new(2.0, 0.12, 14.0),
  Partial::new(3.0, 0.05, 20.0),
];
/// A wooden bar: its overtones sit at 3.93 and 9.2 times the note and fade fast.
const MARIMBA_BAR: &[Partial] = &[
  Partial::new(1.0, 1.0, 10.0),
  Partial::new(3.93, 0.3, 32.0),
  Partial::new(9.2, 0.06, 60.0),
];
/// Chime: G5 then C6, rising.
const CHIME: &[Strike] = &[Strike::new(0.0, 783.99, 0.8), Strike::new(0.13, 1046.5, 1.0)];
const CHIME_SECONDS: f32 = 0.95;
/// Ping: one E6.
const PING: &[Strike] = &[Strike::new(0.0, 1318.51, 1.0)];
const PING_SECONDS: f32 = 0.55;
/// Marimba: C5 then G5.
const MARIMBA: &[Strike] = &[Strike::new(0.0, 523.25, 0.9), Strike::new(0.11, 783.99, 1.0)];
const MARIMBA_SECONDS: f32 = 0.6;
/// Keys: the ringtone's last two notes (C5, E5) in its electric-piano voice.
const KEYS: &[Strike] = &[Strike::new(0.0, 523.25, 1.0), Strike::new(0.11, 659.25, 0.94)];
/// Pop: a bubble whose pitch jumps up as it bursts.
const POP_SECONDS: f32 = 0.16;
const POP_FROM_HZ: f32 = 260.0;
const POP_TO_HZ: f32 = 900.0;
const POP_GLIDE_SECONDS: f32 = 0.015;
const POP_DECAY_PER_SECOND: f32 = 28.0;
/// Knock: two knocks on a wooden door, the second a little softer.
const KNOCKS: &[(f32, f32)] = &[(0.0, 1.0), (0.14, 0.85)];
const KNOCK_SECONDS: f32 = 0.11;
/// Uh-oh: "uh" up high, a short stop, then "oh" a fourth lower, gliding down.
const UH_OH: &[Syllable] = &[
  Syllable {
    start_seconds: 0.0,
    length_seconds: 0.17,
    pitch_hz: (330.0, 345.0),
    // The vowel of "but".
    formants: (
      [
        Formant::new(700.0, 110.0, 1.0),
        Formant::new(1220.0, 120.0, 0.5),
        Formant::new(2600.0, 170.0, 0.18),
      ],
      [
        Formant::new(680.0, 110.0, 1.0),
        Formant::new(1180.0, 120.0, 0.5),
        Formant::new(2600.0, 170.0, 0.18),
      ],
    ),
    release_seconds: 0.03,
  },
  Syllable {
    start_seconds: 0.24,
    length_seconds: 0.34,
    pitch_hz: (252.0, 226.0),
    // The vowel of "go", closing towards "oo".
    formants: (
      [
        Formant::new(560.0, 100.0, 1.0),
        Formant::new(900.0, 110.0, 0.55),
        Formant::new(2450.0, 160.0, 0.12),
      ],
      [
        Formant::new(430.0, 90.0, 1.0),
        Formant::new(760.0, 100.0, 0.5),
        Formant::new(2400.0, 160.0, 0.1),
      ],
    ),
    release_seconds: 0.07,
  },
];
const UH_OH_SECONDS: f32 = 0.6;
/// Harmonics of the voice above this add nothing but hiss.
const VOICE_BANDWIDTH_HZ: f32 = 4500.0;
/// How fast a syllable's voice starts.
const VOICE_ATTACK_SECONDS: f32 = 0.02;
/// The gentle wobble of a held vowel.
const VIBRATO_HZ: f32 = 5.5;
const VIBRATO_DEPTH: f32 = 0.012;

/// Renders one of the app's sounds: mono, 48 kHz, at most about a second, silent at both ends.
pub fn synthesize(sound: BuiltInSound) -> Vec<f32> {
  let samples = match sound {
    BuiltInSound::Chime => struck(BELL, CHIME, CHIME_SECONDS),
    BuiltInSound::Ping => struck(PING_BELL, PING, PING_SECONDS),
    BuiltInSound::Pop => pop(),
    BuiltInSound::Marimba => struck(MARIMBA_BAR, MARIMBA, MARIMBA_SECONDS),
    BuiltInSound::Knock => knocks(),
    BuiltInSound::Keys => keys(),
    BuiltInSound::UhOh => uh_oh(),
  };
  finished(samples)
}

/// Cuts a sound to `max` (a user's own file, "longer files are cut off"), fading out its last
/// moments so the cut doesn't click. Shorter sounds stay as they are.
pub fn cut_to(samples: &mut Vec<f32>, max: Duration) {
  let max_samples = seconds_to_samples(max.as_secs_f32());
  if samples.len() > max_samples {
    samples.truncate(max_samples);
    fade_out(samples, CUT_FADE_SECONDS);
  }
}

/// How long `samples` (48 kHz) play, in milliseconds.
pub fn play_ms(samples: &[f32]) -> u32 {
  u32::try_from(samples.len() as u64 * 1000 / u64::from(SAMPLE_RATE_HZ)).unwrap_or(u32::MAX)
}

/// One partial of a struck tone: its frequency as a multiple of the note, its level and how fast
/// it dies away.
struct Partial {
  ratio: f32,
  level: f32,
  decay_per_second: f32,
}

/// One note: when it starts, its pitch and how hard it is struck.
struct Strike {
  start_seconds: f32,
  frequency_hz: f32,
  velocity: f32,
}

/// One sung syllable: its pitch and vowel (formants) glide from the first value to the second.
struct Syllable {
  start_seconds: f32,
  length_seconds: f32,
  pitch_hz: (f32, f32),
  formants: ([Formant; 3], [Formant; 3]),
  release_seconds: f32,
}

/// A resonance of the vocal tract: where it sits, how wide it is and how loud.
#[derive(Clone, Copy)]
struct Formant {
  frequency_hz: f32,
  bandwidth_hz: f32,
  level: f32,
}

impl Partial {
  const fn new(ratio: f32, level: f32, decay_per_second: f32) -> Self {
    Self {
      ratio,
      level,
      decay_per_second,
    }
  }
}

impl Strike {
  const fn new(start_seconds: f32, frequency_hz: f32, velocity: f32) -> Self {
    Self {
      start_seconds,
      frequency_hz,
      velocity,
    }
  }
}

impl Formant {
  const fn new(frequency_hz: f32, bandwidth_hz: f32, level: f32) -> Self {
    Self {
      frequency_hz,
      bandwidth_hz,
      level,
    }
  }

  /// Linear blend towards `other`; `progress` 0 is `self`, 1 is `other`.
  fn towards(self, other: Formant, progress: f32) -> Formant {
    let blend = |from: f32, to: f32| from + (to - from) * progress;
    Formant::new(
      blend(self.frequency_hz, other.frequency_hz),
      blend(self.bandwidth_hz, other.bandwidth_hz),
      blend(self.level, other.level),
    )
  }

  /// How much of a harmonic at `frequency_hz` passes this resonance (a Lorentzian peak).
  fn gain_at(self, frequency_hz: f32) -> f32 {
    let detune = (frequency_hz - self.frequency_hz) / (self.bandwidth_hz / 2.0);
    self.level / (1.0 + detune * detune)
  }
}

/// Notes of a struck instrument (bell, bar), each a sum of exponentially decaying partials.
fn struck(partials: &[Partial], strikes: &[Strike], length_seconds: f32) -> Vec<f32> {
  let mut samples = vec![0.0f32; seconds_to_samples(length_seconds)];
  for strike in strikes {
    let offset = seconds_to_samples(strike.start_seconds);
    for (index, slot) in samples.iter_mut().skip(offset).enumerate() {
      let time = index as f32 / SAMPLE_RATE_HZ as f32;
      let tone: f32 = partials
        .iter()
        .map(|partial| {
          partial.level
            * (-partial.decay_per_second * time).exp()
            * (TAU * strike.frequency_hz * partial.ratio * time).sin()
        })
        .sum();
      *slot += strike.velocity * attack(time) * tone;
    }
  }
  samples
}

/// A bubble: a sine whose pitch leaps up as it bursts, with a hint of its octave.
fn pop() -> Vec<f32> {
  let mut phase = 0.0f32;
  (0..seconds_to_samples(POP_SECONDS))
    .map(|index| {
      let time = index as f32 / SAMPLE_RATE_HZ as f32;
      let frequency_hz = POP_FROM_HZ + (POP_TO_HZ - POP_FROM_HZ) * (1.0 - (-time / POP_GLIDE_SECONDS).exp());
      phase = (phase + TAU * frequency_hz / SAMPLE_RATE_HZ as f32) % TAU;
      attack(time) * (-POP_DECAY_PER_SECOND * time).exp() * (phase.sin() + 0.15 * (2.0 * phase).sin())
    })
    .collect()
}

/// Two knocks: a low thump whose pitch drops, the wood's resonances and a short, dull click.
fn knocks() -> Vec<f32> {
  let last_start = KNOCKS.iter().fold(0.0f32, |last, (start, _)| last.max(*start));
  let mut samples = vec![0.0f32; seconds_to_samples(last_start + KNOCK_SECONDS)];
  for (start_seconds, velocity) in KNOCKS {
    let offset = seconds_to_samples(*start_seconds);
    for (index, sample) in knock().into_iter().enumerate() {
      if let Some(slot) = samples.get_mut(offset + index) {
        *slot += velocity * sample;
      }
    }
  }
  samples
}

fn knock() -> Vec<f32> {
  // A fixed seed: the knock sounds the same every time (and in every test).
  let mut noise = Xorshift(0x9E37_79B9);
  let mut click = 0.0f32;
  let mut thump_phase = 0.0f32;
  (0..seconds_to_samples(KNOCK_SECONDS))
    .map(|index| {
      let time = index as f32 / SAMPLE_RATE_HZ as f32;
      let thump_hz = 120.0 + 60.0 * (-time / 0.02).exp();
      thump_phase = (thump_phase + TAU * thump_hz / SAMPLE_RATE_HZ as f32) % TAU;
      let thump = (-40.0 * time).exp() * thump_phase.sin();
      let wood = 0.5 * (-55.0 * time).exp() * (TAU * 420.0 * time).sin()
        + 0.2 * (-90.0 * time).exp() * (TAU * 1150.0 * time).sin();
      // Low-passed noise: a dull tap rather than hiss.
      click += 0.25 * (noise.next_unit() - click);
      let tap = 0.6 * (-250.0 * time).exp() * click;
      attack(time) * (thump + wood + tap)
    })
    .collect()
}

/// The ringtone's electric piano, two notes.
fn keys() -> Vec<f32> {
  let mut samples = Vec::new();
  for strike in KEYS {
    let offset = seconds_to_samples(strike.start_seconds);
    let note = electric_piano(strike.frequency_hz, strike.velocity);
    if samples.len() < offset + note.len() {
      samples.resize(offset + note.len(), 0.0);
    }
    for (slot, sample) in samples[offset..].iter_mut().zip(note) {
      *slot += sample;
    }
  }
  samples
}

/// "Uh-oh": each syllable is a voice (harmonics of its pitch, falling off like a real voice's)
/// shaped by the three formants of its vowel.
fn uh_oh() -> Vec<f32> {
  let mut samples = vec![0.0f32; seconds_to_samples(UH_OH_SECONDS)];
  for syllable in UH_OH {
    let offset = seconds_to_samples(syllable.start_seconds);
    for (index, sample) in sing(syllable).into_iter().enumerate() {
      if let Some(slot) = samples.get_mut(offset + index) {
        *slot += sample;
      }
    }
  }
  samples
}

fn sing(syllable: &Syllable) -> Vec<f32> {
  let count = seconds_to_samples(syllable.length_seconds);
  let mut phase = 0.0f32;
  (0..count)
    .map(|index| {
      let time = index as f32 / SAMPLE_RATE_HZ as f32;
      let progress = index as f32 / count as f32;
      let (from_hz, to_hz) = syllable.pitch_hz;
      let vibrato = 1.0 + VIBRATO_DEPTH * (TAU * VIBRATO_HZ * time).sin() * progress;
      let pitch_hz = (from_hz + (to_hz - from_hz) * progress) * vibrato;
      phase = (phase + TAU * pitch_hz / SAMPLE_RATE_HZ as f32) % TAU;
      let (from, to) = &syllable.formants;
      let vowel: [Formant; 3] = std::array::from_fn(|index| from[index].towards(to[index], progress));
      let harmonics = (VOICE_BANDWIDTH_HZ / pitch_hz) as usize;
      let voice: f32 = (1..=harmonics)
        .map(|harmonic| {
          let frequency_hz = pitch_hz * harmonic as f32;
          let gain: f32 = vowel.iter().map(|formant| formant.gain_at(frequency_hz)).sum();
          gain / harmonic as f32 * (harmonic as f32 * phase).sin()
        })
        .sum();
      let fade_in = raised_cosine(time / VOICE_ATTACK_SECONDS);
      let fade_out = raised_cosine((syllable.length_seconds - time) / syllable.release_seconds);
      fade_in * fade_out * voice
    })
    .collect()
}

/// The last steps every sound takes: a fade at the end and the common peak level.
fn finished(mut samples: Vec<f32>) -> Vec<f32> {
  fade_out(&mut samples, TAIL_FADE_SECONDS);
  let peak = samples.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
  if peak > 0.0 {
    samples.iter_mut().for_each(|sample| *sample *= PEAK / peak);
  }
  samples
}

/// Fades the last `seconds` of `samples` out to silence.
fn fade_out(samples: &mut [f32], seconds: f32) {
  let fade = seconds_to_samples(seconds).min(samples.len());
  let start = samples.len() - fade;
  for (index, sample) in samples[start..].iter_mut().enumerate() {
    *sample *= 1.0 - (index + 1) as f32 / fade as f32;
  }
}

/// A struck note's attack: from silence to full within a few milliseconds.
fn attack(time: f32) -> f32 {
  1.0 - (-time / ATTACK_SECONDS).exp()
}

/// 0 at or below 0, 1 at or above 1, a smooth S between.
fn raised_cosine(position: f32) -> f32 {
  let clamped = position.clamp(0.0, 1.0);
  0.5 - 0.5 * (std::f32::consts::PI * clamped).cos()
}

fn seconds_to_samples(seconds: f32) -> usize {
  (seconds * SAMPLE_RATE_HZ as f32).round() as usize
}

/// A tiny deterministic noise source (xorshift32); sound design, not randomness.
struct Xorshift(u32);

impl Xorshift {
  /// The next value in −1..1.
  fn next_unit(&mut self) -> f32 {
    self.0 ^= self.0 << 13;
    self.0 ^= self.0 >> 17;
    self.0 ^= self.0 << 5;
    self.0 as f32 / u32::MAX as f32 * 2.0 - 1.0
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_sound_is_short_audible_and_starts_and_ends_silent() {
    for sound in BuiltInSound::ALL {
      let samples = synthesize(sound);
      let seconds = samples.len() as f32 / SAMPLE_RATE_HZ as f32;
      assert!((0.1..=1.2).contains(&seconds), "{sound:?} lasts {seconds} s");
      assert!(
        (peak(&samples) - PEAK).abs() < 1e-4,
        "{sound:?} peaks at {}",
        peak(&samples)
      );
      assert!(samples[0].abs() < 1e-3, "{sound:?} starts with a click");
      assert!(
        samples[samples.len() - 1].abs() < 1e-3,
        "{sound:?} ends with a click"
      );
    }
  }

  #[test]
  fn no_sound_jumps_between_samples() {
    for sound in BuiltInSound::ALL {
      let samples = synthesize(sound);
      let largest_step = samples
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0f32, f32::max);
      assert!(largest_step < 0.15, "{sound:?} jumps by {largest_step}");
    }
  }

  #[test]
  fn the_sounds_differ_and_are_made_the_same_every_time() {
    let all: Vec<Vec<f32>> = BuiltInSound::ALL.into_iter().map(synthesize).collect();
    for (index, samples) in all.iter().enumerate() {
      for other in &all[index + 1..] {
        assert_ne!(samples, other);
      }
    }
    assert_eq!(synthesize(BuiltInSound::Knock), all[4]);
  }

  #[test]
  fn uh_oh_is_two_syllables_with_a_stop_between_and_the_second_lower() {
    let samples = synthesize(BuiltInSound::UhOh);
    let gap = &samples[seconds_to_samples(0.18)..seconds_to_samples(0.235)];
    assert!(peak(gap) < 0.01, "no stop between the syllables: {}", peak(gap));
    let uh = pitch_hz(&samples[seconds_to_samples(0.06)..seconds_to_samples(0.12)]);
    let oh = pitch_hz(&samples[seconds_to_samples(0.32)..seconds_to_samples(0.38)]);
    assert!((300.0..380.0).contains(&uh), "uh at {uh} Hz");
    assert!((200.0..270.0).contains(&oh), "oh at {oh} Hz");
  }

  #[test]
  fn a_long_sound_is_cut_with_a_fade_and_a_short_one_kept() {
    let mut long = vec![0.5f32; seconds_to_samples(8.0)];
    cut_to(&mut long, Duration::from_secs(5));
    assert_eq!(long.len(), seconds_to_samples(5.0));
    assert!(long[long.len() - 1].abs() < 1e-6);
    assert_eq!(long[0], 0.5);
    assert_eq!(play_ms(&long), 5000);
    let mut short = vec![0.5f32; 100];
    cut_to(&mut short, Duration::from_secs(5));
    assert_eq!(short, vec![0.5f32; 100]);
  }

  fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()))
  }

  /// The fundamental by autocorrelation, searched between 150 and 500 Hz.
  fn pitch_hz(window: &[f32]) -> f32 {
    let rate = SAMPLE_RATE_HZ as f32;
    let lags = (rate / 500.0) as usize..=(rate / 150.0) as usize;
    let best = lags
      .map(|lag| {
        let correlation: f32 = window.iter().zip(&window[lag..]).map(|(a, b)| a * b).sum();
        (lag, correlation)
      })
      .fold(
        (0, f32::MIN),
        |best, next| if next.1 > best.1 { next } else { best },
      );
    rate / best.0 as f32
  }
}
