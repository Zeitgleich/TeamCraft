//! What a call's sound did, for the debug log (#2418): how loud the microphone came in and went
//! out after processing, how loud the speakers played, how much waited for them, and what the echo
//! canceller made of the echo. "Barely audible" and "very loud" in a bug report become numbers:
//! a quiet microphone, gain that didn't raise it, or an echo canceller that took the voice for
//! echo.
//!
//! The first report comes shortly after the call starts, then one a minute.

use std::time::Duration;

use tracing::info;
use web_time::Instant;

use crate::audio_frame::SAMPLE_RATE_HZ;
use crate::audio_processing::EchoStats;

/// The first report: long enough for the echo canceller and gain control to settle.
const FIRST_REPORT_AFTER: Duration = Duration::from_secs(15);
/// Then one a minute.
const REPORT_EVERY: Duration = Duration::from_secs(60);
/// Frames quieter than this count as silence (a quiet room is about -60 dBFS).
const ACTIVE_FLOOR_DBFS: f64 = -60.0;

/// When the next report is due.
#[derive(Debug)]
pub struct AudioReportClock {
  next_at: Instant,
}

/// One signal's level over a report's period, in 20 ms frames.
#[derive(Debug, Clone, Default)]
pub struct LevelWindow {
  frames: u64,
  active_frames: u64,
  /// Sum of squares and sample count of the active frames only: silence between sentences
  /// doesn't drag the level of the speech down.
  active_sum_of_squares: f64,
  active_samples: u64,
  peak: f32,
}

/// A [`LevelWindow`] read out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelSummary {
  /// RMS of the frames above the silence floor; `None` if all were silent.
  pub maybe_active_rms_dbfs: Option<f64>,
  pub maybe_peak_dbfs: Option<f64>,
  /// Share of the frames above the silence floor (0 to 100).
  pub active_percent: u8,
}

/// Everything one report logs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioReport {
  /// The microphone as the device delivered it.
  pub microphone: LevelSummary,
  /// The microphone after echo cancellation, noise suppression and gain control: what is sent.
  pub sent: LevelSummary,
  /// What the speakers were given.
  pub played: LevelSummary,
  /// Samples waiting for the speakers at the time of the report, and the underruns so far.
  pub maybe_playback: Option<PlaybackQueue>,
  pub echo: EchoStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackQueue {
  pub queued_samples: usize,
  pub underruns: u64,
}

impl AudioReportClock {
  pub fn new(start: Instant) -> Self {
    Self {
      next_at: start + FIRST_REPORT_AFTER,
    }
  }

  /// Whether a report is due at `now`; if so the next one is planned.
  pub fn is_due(&mut self, now: Instant) -> bool {
    if now < self.next_at {
      return false;
    }
    self.next_at = now + REPORT_EVERY;
    true
  }
}

impl LevelWindow {
  /// Adds one frame of mono samples (-1.0..=1.0).
  pub fn push(&mut self, frame: &[f32]) {
    if frame.is_empty() {
      return;
    }
    self.frames += 1;
    let sum_of_squares: f64 = frame.iter().map(|sample| f64::from(*sample).powi(2)).sum();
    if dbfs_of_mean_square(sum_of_squares / frame.len() as f64).is_some_and(|dbfs| dbfs > ACTIVE_FLOOR_DBFS) {
      self.active_frames += 1;
      self.active_sum_of_squares += sum_of_squares;
      self.active_samples += frame.len() as u64;
    }
    self.peak = frame
      .iter()
      .fold(self.peak, |peak, sample| peak.max(sample.abs()));
  }

  /// The period's summary; the window starts empty again.
  pub fn take(&mut self) -> LevelSummary {
    let window = std::mem::take(self);
    let maybe_active_rms_dbfs = (window.active_samples > 0)
      .then(|| dbfs_of_mean_square(window.active_sum_of_squares / window.active_samples as f64))
      .flatten();
    let active_percent = (window.active_frames * 100)
      .checked_div(window.frames)
      .map_or(0, |percent| percent.min(100) as u8);
    LevelSummary {
      maybe_active_rms_dbfs,
      maybe_peak_dbfs: dbfs_of_mean_square(f64::from(window.peak).powi(2)),
      active_percent,
    }
  }
}

impl AudioReport {
  /// Writes the report into the debug log (`info`, which it keeps). Every value is a number,
  /// which the debug log's redaction keeps whatever the field's name; a value that isn't there
  /// (silence, no estimate yet, no speakers) is left out of the line.
  pub fn log(&self) {
    let maybe_queued_ms = self
      .maybe_playback
      .map(|queue| queue.queued_samples as u64 * 1000 / u64::from(SAMPLE_RATE_HZ));
    info!(
      microphone_dbfs = tenths(self.microphone.maybe_active_rms_dbfs),
      microphone_peak_dbfs = tenths(self.microphone.maybe_peak_dbfs),
      microphone_active_percent = self.microphone.active_percent,
      sent_dbfs = tenths(self.sent.maybe_active_rms_dbfs),
      sent_peak_dbfs = tenths(self.sent.maybe_peak_dbfs),
      sent_active_percent = self.sent.active_percent,
      played_dbfs = tenths(self.played.maybe_active_rms_dbfs),
      played_peak_dbfs = tenths(self.played.maybe_peak_dbfs),
      played_active_percent = self.played.active_percent,
      playback_queued_ms = maybe_queued_ms,
      playback_underruns = self.maybe_playback.map(|queue| queue.underruns),
      echo_delay_ms = self.echo.maybe_delay_ms,
      echo_return_loss_db = tenths(self.echo.maybe_echo_return_loss_db),
      echo_return_loss_enhancement_db = tenths(self.echo.maybe_echo_return_loss_enhancement_db),
      residual_echo_likelihood = hundredths(self.echo.maybe_residual_echo_likelihood),
      "call audio"
    );
  }
}

/// Decibels with one decimal; `None` stays out of the line, as does a value that isn't finite.
fn tenths(maybe_value: Option<f64>) -> Option<f64> {
  rounded(maybe_value, 10.0)
}

/// A likelihood with two decimals.
fn hundredths(maybe_value: Option<f64>) -> Option<f64> {
  rounded(maybe_value, 100.0)
}

fn rounded(maybe_value: Option<f64>, scale: f64) -> Option<f64> {
  maybe_value
    .filter(|value| value.is_finite())
    .map(|value| (value * scale).round() / scale)
}

/// `None` for digital silence.
fn dbfs_of_mean_square(mean_square: f64) -> Option<f64> {
  // 10·log10 of the mean square is 20·log10 of the RMS.
  (mean_square > 0.0).then(|| 10.0 * mean_square.log10())
}

#[cfg(test)]
mod tests {
  use super::*;

  mod levels {
    use super::*;

    #[test]
    fn silence_between_sentences_does_not_lower_the_speech_level() {
      let mut window = LevelWindow::default();
      // A sine of amplitude 0.1 has an RMS of 0.0707: -23 dBFS. Half the frames are silent.
      for index in 0..100 {
        if index % 2 == 0 {
          window.push(&sine(0.1));
        } else {
          window.push(&[0.0; 960]);
        }
      }
      let summary = window.take();
      let rms = summary.maybe_active_rms_dbfs.unwrap();
      assert!((rms - -23.0).abs() < 0.2, "{rms}");
      assert!((summary.maybe_peak_dbfs.unwrap() - -20.0).abs() < 0.1);
      assert_eq!(summary.active_percent, 50);
    }

    #[test]
    fn a_window_of_silence_has_no_level_and_take_starts_over() {
      let mut window = LevelWindow::default();
      window.push(&[0.0; 960]);
      // Below the floor: there, but not active.
      window.push(&[0.000_1; 960]);
      let summary = window.take();
      assert_eq!(summary.maybe_active_rms_dbfs, None);
      assert_eq!(summary.active_percent, 0);
      assert!(summary.maybe_peak_dbfs.unwrap() < -79.0);
      let empty = window.take();
      assert_eq!(
        empty,
        LevelSummary {
          maybe_active_rms_dbfs: None,
          maybe_peak_dbfs: None,
          active_percent: 0,
        }
      );
    }

    fn sine(amplitude: f32) -> Vec<f32> {
      (0..960)
        .map(|i| amplitude * (2.0 * std::f32::consts::PI * 500.0 * i as f32 / 48_000.0).sin())
        .collect()
    }
  }

  /// The debug log redacts text fields by name but keeps every number: the report's values are
  /// numbers, and the ones that aren't there are left out instead of written as text.
  #[test]
  fn the_report_logs_only_numbers() {
    use std::sync::{Arc, Mutex};

    use tracing::field::{Field, Visit};
    use tracing_subscriber::layer::{Context, SubscriberExt};

    #[derive(Default)]
    struct Fields {
      numbers: Vec<String>,
      texts: Vec<String>,
    }
    struct Recorder(Arc<Mutex<Fields>>);
    impl Visit for Fields {
      fn record_f64(&mut self, field: &Field, _: f64) {
        self.numbers.push(field.name().to_owned());
      }
      fn record_u64(&mut self, field: &Field, _: u64) {
        self.numbers.push(field.name().to_owned());
      }
      fn record_i64(&mut self, field: &Field, _: i64) {
        self.numbers.push(field.name().to_owned());
      }
      fn record_debug(&mut self, field: &Field, _: &dyn std::fmt::Debug) {
        self.texts.push(field.name().to_owned());
      }
    }
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Recorder {
      fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        event.record(&mut *self.0.lock().unwrap());
      }
    }

    let fields = Arc::new(Mutex::new(Fields::default()));
    let subscriber = tracing_subscriber::registry().with(Recorder(Arc::clone(&fields)));
    let mut microphone = LevelWindow::default();
    microphone.push(&[0.01; 960]);
    let report = AudioReport {
      microphone: microphone.take(),
      sent: LevelWindow::default().take(),
      played: LevelWindow::default().take(),
      maybe_playback: Some(PlaybackQueue {
        queued_samples: 960,
        underruns: 3,
      }),
      echo: EchoStats {
        maybe_delay_ms: Some(48),
        ..EchoStats::default()
      },
    };
    tracing::subscriber::with_default(subscriber, || report.log());
    let fields = fields.lock().unwrap();
    assert_eq!(fields.texts, ["message"]);
    for expected in [
      "microphone_dbfs",
      "microphone_peak_dbfs",
      "sent_active_percent",
      "playback_queued_ms",
      "playback_underruns",
      "echo_delay_ms",
    ] {
      assert!(
        fields.numbers.iter().any(|name| name == expected),
        "{expected} missing"
      );
    }
    // Silence and missing estimates stay out of the line.
    assert!(!fields.numbers.iter().any(|name| name == "sent_dbfs"));
    assert!(!fields.numbers.iter().any(|name| name == "echo_return_loss_db"));
  }

  #[test]
  fn the_first_report_comes_after_settling_then_one_a_minute() {
    let start = Instant::now();
    let mut clock = AudioReportClock::new(start);
    assert!(!clock.is_due(start));
    assert!(!clock.is_due(start + FIRST_REPORT_AFTER - Duration::from_millis(1)));
    assert!(clock.is_due(start + FIRST_REPORT_AFTER));
    assert!(!clock.is_due(start + FIRST_REPORT_AFTER + Duration::from_secs(30)));
    assert!(clock.is_due(start + FIRST_REPORT_AFTER + REPORT_EVERY));
  }

  #[test]
  fn values_are_rounded_and_only_finite_ones_logged() {
    assert_eq!(tenths(Some(-23.456)), Some(-23.5));
    assert_eq!(hundredths(Some(0.123_4)), Some(0.12));
    assert_eq!(tenths(None), None);
    assert_eq!(tenths(Some(f64::NEG_INFINITY)), None);
  }
}
