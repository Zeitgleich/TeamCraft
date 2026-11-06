//! Voice messages (#204), what every app shares: the waveform the sender measures while
//! recording, the playback speeds, and the name the recorded file gets.
//!
//! A voice message is an ordinary encrypted audio attachment whose details (inside the message's
//! ciphertext, e2ee.md §4.5) also carry its duration and its waveform. An app that doesn't know
//! voice messages shows the plain audio file; the server sees an attachment of some size.

use serde::{Deserialize, Serialize};

/// How many bars a recording's waveform has.
pub const WAVEFORM_BARS: usize = 64;
/// The most bars a received waveform may have; a longer one is dropped (the file stays a plain
/// audio file), so a sender can't make receivers draw thousands of bars.
pub const MAX_WAVEFORM_BARS: usize = 128;
/// Quieter than this (in dB below full scale) is the floor of the waveform.
const FLOOR_DB: f32 = -50.0;
/// The lowest bar drawn, so silence still shows as a line.
const MIN_BAR: u8 = 8;

/// A recording's loudness over time, one byte per bar (0 silent, 255 the loudest moment of the
/// take). Scaled to the take's own peak, so a quiet speaker's message still has a shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waveform(Vec<u8>);

/// The speeds a voice message plays at; the choice is remembered on the device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackSpeed {
  #[default]
  Normal,
  OneAndHalf,
  Double,
}

/// Why a recording stopped before the user sent it. The take is kept: the user listens, sends or
/// discards it; nothing half-recorded is ever sent on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingStop {
  /// The maximum length (the workspace's voice message policy) was reached.
  LimitReached,
  /// A call started, or the phone rang.
  Call,
  /// The app went to the background, the screen locked, or the window was hidden.
  Background,
  /// The microphone went away (unplugged, taken by another app).
  MicrophoneLost,
  /// The user left the conversation the recording is for.
  Left,
}

/// A recording in the desktop app's shell as the composer polls it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecordingStatus {
  /// The meter's level, 0.0 to 1.0.
  pub level: f32,
  pub elapsed_ms: u64,
  /// Set once the recording stopped by itself: the composer then finishes it.
  #[serde(rename = "stop", default, skip_serializing_if = "Option::is_none")]
  pub maybe_stop: Option<RecordingStop>,
}

/// A take the desktop app's shell finished, kept in its private folder until sent or discarded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellTake {
  /// The take's file in the shell's recording folder; only the shell reads it.
  pub path: String,
  pub duration_ms: u64,
  pub waveform: Vec<u8>,
  #[serde(rename = "stop", default, skip_serializing_if = "Option::is_none")]
  pub maybe_stop: Option<RecordingStop>,
}

/// Why the desktop app's shell couldn't start a recording; the composer words it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingRefusal {
  /// No microphone, or the system refused it (permission).
  Microphone,
  /// Another recording runs (another window).
  Busy,
  Failed,
}

impl RecordingRefusal {
  /// The error text a refused command answers with, and back.
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Microphone => "voice_microphone",
      Self::Busy => "voice_busy",
      Self::Failed => "voice_failed",
    }
  }

  pub fn parse(text: &str) -> Self {
    match text {
      "voice_microphone" => Self::Microphone,
      "voice_busy" => Self::Busy,
      _ => Self::Failed,
    }
  }
}

impl Waveform {
  /// The waveform of a take from its levels, one linear amplitude (0.0 to 1.0, RMS or peak) per
  /// short frame in recording order. Up to [`WAVEFORM_BARS`] bars; a take shorter than that has
  /// one bar per frame. `None` for a take without frames.
  pub fn from_levels(levels: &[f32]) -> Option<Self> {
    if levels.is_empty() {
      return None;
    }
    let bars = levels.len().min(WAVEFORM_BARS);
    let loudness: Vec<f32> = (0..bars)
      .map(|bar| {
        let start = bar * levels.len() / bars;
        let end = ((bar + 1) * levels.len() / bars).max(start + 1);
        let peak = levels[start..end].iter().copied().fold(0.0_f32, f32::max);
        loudness_of(peak)
      })
      .collect();
    let loudest = loudness.iter().copied().fold(0.0_f32, f32::max);
    let bytes = loudness
      .iter()
      .map(|&value| match loudest > 0.0 {
        true => (MIN_BAR as f32 + (value / loudest) * f32::from(u8::MAX - MIN_BAR)).round() as u8,
        false => MIN_BAR,
      })
      .collect();
    Some(Self(bytes))
  }

  /// A waveform received in a message, if it is one: 1 to [`MAX_WAVEFORM_BARS`] bars.
  pub fn checked(bytes: &[u8]) -> Option<Self> {
    (1..=MAX_WAVEFORM_BARS)
      .contains(&bytes.len())
      .then(|| Self(bytes.to_vec()))
  }

  /// `count` bar heights from 0.0 to 1.0 for drawing: the waveform stretched or squeezed to the
  /// bars that fit (each drawn bar is the loudest of the stored bars it covers).
  pub fn heights(&self, count: usize) -> Vec<f32> {
    let stored = &self.0;
    (0..count)
      .map(|bar| {
        let start = bar * stored.len() / count;
        // `start` < len as bar < count; `checked` keeps the waveform non-empty.
        let end = ((bar + 1) * stored.len() / count).max(start + 1);
        let peak = stored[start..end].iter().copied().max().unwrap_or(0);
        f32::from(peak) / f32::from(u8::MAX)
      })
      .collect()
  }

  pub fn as_bytes(&self) -> &[u8] {
    &self.0
  }

  pub fn into_bytes(self) -> Vec<u8> {
    self.0
  }
}

impl PlaybackSpeed {
  /// The rate the player plays at.
  pub fn rate(self) -> f64 {
    match self {
      Self::Normal => 1.0,
      Self::OneAndHalf => 1.5,
      Self::Double => 2.0,
    }
  }

  /// The speed a press on the speed button goes to.
  pub fn next(self) -> Self {
    match self {
      Self::Normal => Self::OneAndHalf,
      Self::OneAndHalf => Self::Double,
      Self::Double => Self::Normal,
    }
  }

  /// What the speed button shows: a number, the same in every language.
  pub fn label(self) -> &'static str {
    match self {
      Self::Normal => "1×",
      Self::OneAndHalf => "1.5×",
      Self::Double => "2×",
    }
  }
}

/// The name of a recorded file, for clients that show it as a plain file and for downloads:
/// `voice-message-20261105-140312.ogg`. `extension` without the dot; the time is the local time
/// of the recording as `(year, month, day, hour, minute, second)`.
pub fn file_name(local_time: (i32, u32, u32, u32, u32, u32), extension: &str) -> String {
  let (year, month, day, hour, minute, second) = local_time;
  format!("voice-message-{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}.{extension}")
}

/// A level as loudness from 0.0 (at or below the floor) to 1.0 (full scale), on a dB scale like the
/// ear hears it.
fn loudness_of(level: f32) -> f32 {
  if !level.is_finite() || level <= 0.0 {
    return 0.0;
  }
  let db = 20.0 * level.min(1.0).log10();
  ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
  use super::*;

  mod waveform {
    use super::*;

    #[test]
    fn has_one_bar_per_frame_for_short_takes_and_64_for_long_ones() {
      assert_eq!(Waveform::from_levels(&[0.5; 10]).unwrap().as_bytes().len(), 10);
      assert_eq!(
        Waveform::from_levels(&[0.5; 3_000]).unwrap().as_bytes().len(),
        WAVEFORM_BARS
      );
      assert_eq!(Waveform::from_levels(&[]), None);
    }

    #[test]
    fn scales_to_the_loudest_moment_of_the_take() {
      let waveform = Waveform::from_levels(&[0.001, 0.01, 0.05]).unwrap();
      let bytes = waveform.as_bytes();
      assert_eq!(bytes[2], u8::MAX, "a quiet take still reaches the top");
      assert!(bytes[0] < bytes[1] && bytes[1] < bytes[2]);
    }

    #[test]
    fn silence_and_garbage_levels_are_the_floor() {
      let waveform = Waveform::from_levels(&[0.0, f32::NAN, -1.0]).unwrap();
      assert!(waveform.as_bytes().iter().all(|&bar| bar == MIN_BAR));
    }

    #[test]
    fn received_waveforms_are_bounded() {
      assert!(Waveform::checked(&[]).is_none());
      assert!(Waveform::checked(&[1; MAX_WAVEFORM_BARS]).is_some());
      assert!(Waveform::checked(&[1; MAX_WAVEFORM_BARS + 1]).is_none());
    }

    #[test]
    fn heights_fit_any_bar_count() {
      let waveform = Waveform::checked(&[0, 255, 0, 51]).unwrap();
      assert_eq!(waveform.heights(2), vec![1.0, 0.2]);
      assert_eq!(waveform.heights(8).len(), 8);
      assert_eq!(waveform.heights(1), vec![1.0]);
    }
  }

  #[test]
  fn speeds_cycle_and_serialize_stably() {
    assert_eq!(PlaybackSpeed::Normal.next().next().next(), PlaybackSpeed::Normal);
    assert_eq!(PlaybackSpeed::OneAndHalf.rate(), 1.5);
    assert_eq!(
      serde_json::to_string(&PlaybackSpeed::OneAndHalf).unwrap(),
      "\"one_and_half\""
    );
    assert_eq!(
      serde_json::to_string(&RecordingStop::LimitReached).unwrap(),
      "\"limit_reached\""
    );
  }

  #[test]
  fn file_names_sort_by_time() {
    assert_eq!(
      file_name((2026, 11, 5, 14, 3, 12), "ogg"),
      "voice-message-20261105-140312.ogg"
    );
  }
}
