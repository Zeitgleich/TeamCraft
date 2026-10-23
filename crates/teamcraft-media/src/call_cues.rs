//! Call status sounds: short tones that tell the user what the connection does, played locally
//! into the call's own playback (never sent), so they take the call's audio route – earpiece,
//! speaker, headset, car – on every platform.
//!
//! - **Reconnecting:** while this device's connection is lost, a soft falling two-tone "du-dun"
//!   repeats every two seconds until the call is back or over.
//! - **Reconnected:** a short rising two-tone once the call is back.
//! - **Connection lost:** three falling tones when the call ends because rejoining failed.
//! - **Someone else reconnecting:** one quieter "du-dun" when another participant's connection drops.
//!
//! The tones are synthesized (sine waves with raised-cosine edges, so they don't click); there is
//! no sound file. They stay well below speech level and play regardless of mute or deafen: they
//! are status, not call audio. [`StatusCues`] turns them down to the own connection, or off.
//! Pure: the drivers feed engine events and pull 20 ms frames.

use crate::audio_frame::SAMPLE_RATE_HZ;
use crate::call_engine::CallEngineEvent;

/// Peak level of the own connection's cues: about −18 dBFS, well below speech.
const CUE_AMPLITUDE: f32 = 0.125;
/// Others' reconnecting cue is quieter: it is about someone else.
const OTHERS_CUE_AMPLITUDE: f32 = 0.06;
/// Fade in and out of every note, so the tones don't click.
const EDGE_SECONDS: f32 = 0.012;

/// The "du-dun": a falling fourth (D5 → A4), then silence until the next repetition.
const RECONNECTING: &[Note] = &[
  Note::tone(587.33, 0.14),
  Note::rest(0.07),
  Note::tone(440.0, 0.22),
  Note::rest(1.57),
];
const RECONNECTED: &[Note] = &[
  Note::tone(440.0, 0.11),
  Note::rest(0.04),
  Note::tone(659.25, 0.18),
];
const CONNECTION_LOST: &[Note] = &[
  Note::tone(523.25, 0.15),
  Note::rest(0.05),
  Note::tone(392.0, 0.15),
  Note::rest(0.05),
  Note::tone(329.63, 0.3),
];
const OTHER_RECONNECTING: &[Note] = &[
  Note::tone(587.33, 0.12),
  Note::rest(0.06),
  Note::tone(440.0, 0.18),
];

/// Which status sounds play.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StatusCues {
  /// The own connection's cues and the quieter one for others reconnecting.
  #[default]
  All,
  /// Only the own connection's cues.
  OwnOnly,
  Off,
}

/// One status sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallCue {
  /// Repeats while this device rejoins.
  Reconnecting,
  Reconnected,
  /// The call ended because rejoining failed.
  ConnectionLost,
  OtherReconnecting,
}

/// Plays cues into 20 ms playback frames.
#[derive(Debug)]
pub struct CuePlayer {
  setting: StatusCues,
  maybe_playing: Option<Playing>,
}

#[derive(Debug)]
struct Playing {
  cue: CallCue,
  samples: Vec<f32>,
  position: usize,
}

/// A tone (frequency in Hz) or a rest, with its length in seconds.
#[derive(Debug, Clone, Copy)]
struct Note {
  maybe_frequency_hz: Option<f32>,
  seconds: f32,
}

impl CuePlayer {
  pub fn new(setting: StatusCues) -> Self {
    Self {
      setting,
      maybe_playing: None,
    }
  }

  /// Changes the setting; a cue that is no longer wanted stops at once.
  pub fn set_setting(&mut self, setting: StatusCues) {
    self.setting = setting;
    if let Some(playing) = &self.maybe_playing
      && !self.is_wanted(playing.cue)
    {
      self.maybe_playing = None;
    }
  }

  /// Starts, changes or stops cues for what the engine reported.
  pub fn on_event(&mut self, event: &CallEngineEvent) {
    match event {
      CallEngineEvent::Reconnecting => self.play(CallCue::Reconnecting),
      CallEngineEvent::Reconnected => self.play(CallCue::Reconnected),
      // The own reconnecting loop is the more important news.
      CallEngineEvent::ParticipantReconnecting(_) if !self.is_playing(CallCue::Reconnecting) => {
        self.play(CallCue::OtherReconnecting);
      },
      _ => {},
    }
  }

  /// Starts `cue`, replacing the one playing.
  pub fn play(&mut self, cue: CallCue) {
    if !self.is_wanted(cue) {
      return;
    }
    self.maybe_playing = Some(Playing {
      cue,
      samples: synthesize(cue),
      position: 0,
    });
  }

  pub fn is_playing(&self, cue: CallCue) -> bool {
    self
      .maybe_playing
      .as_ref()
      .is_some_and(|playing| playing.cue == cue)
  }

  /// The cue's next samples (mono, 48 kHz) for one playback frame of `len` samples; silence when
  /// nothing plays. The reconnecting cue repeats until something else replaces it.
  pub fn next_samples(&mut self, len: usize) -> Vec<f32> {
    let mut out = vec![0.0; len];
    let Some(playing) = &mut self.maybe_playing else {
      return out;
    };
    let mut filled = 0;
    let mut is_done = false;
    while filled < len {
      if playing.position == playing.samples.len() {
        if playing.cue != CallCue::Reconnecting {
          is_done = true;
          break;
        }
        playing.position = 0;
      }
      let count = (len - filled).min(playing.samples.len() - playing.position);
      out[filled..filled + count]
        .copy_from_slice(&playing.samples[playing.position..playing.position + count]);
      playing.position += count;
      filled += count;
    }
    if is_done {
      self.maybe_playing = None;
    }
    out
  }

  fn is_wanted(&self, cue: CallCue) -> bool {
    match self.setting {
      StatusCues::All => true,
      StatusCues::OwnOnly => cue != CallCue::OtherReconnecting,
      StatusCues::Off => false,
    }
  }
}

/// The samples of one pass of `cue` (mono, 48 kHz).
pub fn synthesize(cue: CallCue) -> Vec<f32> {
  let (notes, amplitude) = match cue {
    CallCue::Reconnecting => (RECONNECTING, CUE_AMPLITUDE),
    CallCue::Reconnected => (RECONNECTED, CUE_AMPLITUDE),
    CallCue::ConnectionLost => (CONNECTION_LOST, CUE_AMPLITUDE),
    CallCue::OtherReconnecting => (OTHER_RECONNECTING, OTHERS_CUE_AMPLITUDE),
  };
  let mut samples = Vec::new();
  for note in notes {
    let count = (note.seconds * SAMPLE_RATE_HZ as f32).round() as usize;
    match note.maybe_frequency_hz {
      Some(frequency_hz) => samples.extend((0..count).map(|index| {
        let time = index as f32 / SAMPLE_RATE_HZ as f32;
        let envelope = edge(time).min(edge(note.seconds - time));
        amplitude * envelope * (std::f32::consts::TAU * frequency_hz * time).sin()
      })),
      None => samples.extend(std::iter::repeat_n(0.0, count)),
    }
  }
  samples
}

impl Note {
  const fn tone(frequency_hz: f32, seconds: f32) -> Self {
    Self {
      maybe_frequency_hz: Some(frequency_hz),
      seconds,
    }
  }

  const fn rest(seconds: f32) -> Self {
    Self {
      maybe_frequency_hz: None,
      seconds,
    }
  }
}

/// Raised-cosine ramp over [`EDGE_SECONDS`]: 0 at the note's edge, 1 inside.
fn edge(seconds_from_edge: f32) -> f32 {
  if seconds_from_edge >= EDGE_SECONDS {
    return 1.0;
  }
  let progress = seconds_from_edge.max(0.0) / EDGE_SECONDS;
  0.5 - 0.5 * (std::f32::consts::PI * progress).cos()
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{DeviceId, Id, Snowflake};

  use super::*;

  const FRAME: usize = 960;

  #[test]
  fn cues_stay_below_speech_and_start_and_end_silently() {
    for cue in [
      CallCue::Reconnecting,
      CallCue::Reconnected,
      CallCue::ConnectionLost,
      CallCue::OtherReconnecting,
    ] {
      let samples = synthesize(cue);
      let peak = samples.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
      assert!(peak > 0.03 && peak <= CUE_AMPLITUDE, "{cue:?} peaks at {peak}");
      // No clicks: every note fades in and out.
      assert!(
        samples[0].abs() < 1e-3 && samples[samples.len() - 1].abs() < 1e-3,
        "{cue:?}"
      );
      let largest_step = samples
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0f32, f32::max);
      assert!(largest_step < 0.02, "{cue:?} jumps by {largest_step}");
    }
    let others = synthesize(CallCue::OtherReconnecting);
    assert!(others.iter().all(|sample| sample.abs() <= OTHERS_CUE_AMPLITUDE));
  }

  #[test]
  fn reconnecting_repeats_until_the_call_is_back() {
    let mut player = CuePlayer::new(StatusCues::All);
    player.on_event(&CallEngineEvent::Reconnecting);
    let period = synthesize(CallCue::Reconnecting).len();
    // Three periods' worth of frames: still playing, and the second pass sounds like the first.
    let frames: Vec<Vec<f32>> = (0..3 * period / FRAME)
      .map(|_| player.next_samples(FRAME))
      .collect();
    assert!(player.is_playing(CallCue::Reconnecting));
    let flat: Vec<f32> = frames.concat();
    assert_eq!(flat[..FRAME], flat[period..period + FRAME]);
    assert!(flat[..FRAME].iter().any(|sample| sample.abs() > 0.01));
    // Back: the rising cue plays once, then silence.
    player.on_event(&CallEngineEvent::Reconnected);
    assert!(player.is_playing(CallCue::Reconnected));
    let played: Vec<f32> = (0..20).flat_map(|_| player.next_samples(FRAME)).collect();
    assert!(played.iter().any(|sample| sample.abs() > 0.01));
    assert!(!player.is_playing(CallCue::Reconnected));
    assert!(player.next_samples(FRAME).iter().all(|sample| *sample == 0.0));
  }

  #[test]
  fn someone_else_reconnecting_plays_once_and_never_over_the_own_loop() {
    let mut player = CuePlayer::new(StatusCues::All);
    player.on_event(&CallEngineEvent::ParticipantReconnecting(device(2)));
    assert!(player.is_playing(CallCue::OtherReconnecting));
    for _ in 0..30 {
      player.next_samples(FRAME);
    }
    assert!(!player.is_playing(CallCue::OtherReconnecting));
    player.on_event(&CallEngineEvent::Reconnecting);
    player.on_event(&CallEngineEvent::ParticipantReconnecting(device(3)));
    assert!(player.is_playing(CallCue::Reconnecting));
  }

  #[test]
  fn setting_limits_or_silences_the_cues() {
    let mut own_only = CuePlayer::new(StatusCues::OwnOnly);
    own_only.on_event(&CallEngineEvent::ParticipantReconnecting(device(2)));
    assert!(!own_only.is_playing(CallCue::OtherReconnecting));
    own_only.on_event(&CallEngineEvent::Reconnecting);
    assert!(own_only.is_playing(CallCue::Reconnecting));
    // Turned off mid-loop: silent at once.
    own_only.set_setting(StatusCues::Off);
    assert!(own_only.next_samples(FRAME).iter().all(|sample| *sample == 0.0));
    own_only.play(CallCue::ConnectionLost);
    assert!(!own_only.is_playing(CallCue::ConnectionLost));
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
