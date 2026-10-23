//! Whether the user is looking at this desktop or web session (notifications.md, "Attention").
//!
//! While a session is attentive the server holds phone pushes back for the grace delay, so the report
//! must be honest: focused and visible window, recent input in the app, little OS idle time and an
//! unlocked screen. [`is_attentive`] is the rule; [`AttentionReporter`] decides when a report goes
//! out: changes only, and "away" only once it lasted [`ATTENTION_AWAY_DELAY_SECS`], so the server
//! learns coarse transitions, not a timeline (e2ee.md §7). The session's gateway heartbeats keep an
//! attentive report alive. Both are pure; the browser wiring lives in `components::app`.

use std::time::Duration;

use teamcraft_proto::ATTENTION_AWAY_DELAY_SECS;

use crate::model::TimestampMs;

/// Default for "input in the app within" (notifications.md, `attention_input_secs`).
pub const DEFAULT_ATTENTION_INPUT_SECS: u32 = 120;

const MS_PER_SECOND: i64 = 1000;

/// What the session knows about the user right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttentionSignals {
  pub is_focused: bool,
  pub is_visible: bool,
  /// The last pointer, key, touch or wheel input inside the app.
  pub last_input_at: TimestampMs,
  /// Time since the last input anywhere on the computer; `None` where the platform does not tell.
  pub maybe_os_idle: Option<Duration>,
  pub is_screen_locked: bool,
}

/// Decides when the session reports its attention.
#[derive(Debug, Default)]
pub struct AttentionReporter {
  maybe_reported: Option<bool>,
  /// Since when the user is not attentive while the last report still says attentive.
  maybe_away_since: Option<TimestampMs>,
}

/// The rule: every condition has to hold, an unknown OS idle time counts as no objection.
pub fn is_attentive(signals: &AttentionSignals, now: TimestampMs, input_window: Duration) -> bool {
  let window_ms = i64::try_from(input_window.as_millis()).unwrap_or(i64::MAX);
  let has_recent_input = now.saturating_sub(signals.last_input_at) < window_ms;
  let is_os_active = signals.maybe_os_idle.is_none_or(|idle| idle < input_window);
  signals.is_focused && signals.is_visible && has_recent_input && is_os_active && !signals.is_screen_locked
}

impl AttentionReporter {
  /// The report to send now, if any: the first state, becoming attentive at once, and being away
  /// once it lasted the away delay.
  pub fn next(&mut self, is_attentive: bool, now: TimestampMs) -> Option<bool> {
    match self.maybe_reported {
      None => self.send(is_attentive),
      Some(reported) if reported == is_attentive => {
        self.maybe_away_since = None;
        None
      },
      Some(_) if is_attentive => self.send(true),
      Some(_) => {
        let since = *self.maybe_away_since.get_or_insert(now);
        (now.saturating_sub(since) >= away_delay_ms())
          .then(|| self.send(false))
          .flatten()
      },
    }
  }

  /// The session goes away (window closed, attention turned off): "not attentive" now, unless that
  /// is what the server already knows.
  pub fn leave(&mut self) -> Option<bool> {
    (self.maybe_reported != Some(false))
      .then(|| self.send(false))
      .flatten()
  }

  /// The last report a reconnected gateway should hear again.
  pub fn last_report(&self) -> Option<bool> {
    self.maybe_reported
  }

  fn send(&mut self, is_attentive: bool) -> Option<bool> {
    self.maybe_reported = Some(is_attentive);
    self.maybe_away_since = None;
    Some(is_attentive)
  }
}

fn away_delay_ms() -> i64 {
  i64::try_from(ATTENTION_AWAY_DELAY_SECS).unwrap_or(i64::MAX) * MS_PER_SECOND
}

#[cfg(test)]
mod tests {
  use super::*;

  const NOW: TimestampMs = 1_792_540_800_000;
  const WINDOW: Duration = Duration::from_secs(DEFAULT_ATTENTION_INPUT_SECS as u64);

  mod rule {
    use super::*;

    #[test]
    fn a_focused_visible_session_with_recent_input_is_attentive() {
      assert!(is_attentive(&looking(), NOW, WINDOW));
    }

    #[test]
    fn every_missing_condition_makes_the_session_inattentive() {
      let cases = [
        AttentionSignals {
          is_focused: false,
          ..looking()
        },
        AttentionSignals {
          is_visible: false,
          ..looking()
        },
        AttentionSignals {
          last_input_at: NOW - 120_000,
          ..looking()
        },
        AttentionSignals {
          maybe_os_idle: Some(WINDOW),
          ..looking()
        },
        AttentionSignals {
          is_screen_locked: true,
          ..looking()
        },
      ];
      for signals in cases {
        assert!(!is_attentive(&signals, NOW, WINDOW), "{signals:?}");
      }
    }

    #[test]
    fn input_just_inside_the_window_counts() {
      let signals = AttentionSignals {
        last_input_at: NOW - 119_999,
        maybe_os_idle: Some(Duration::from_millis(119_999)),
        ..looking()
      };
      assert!(is_attentive(&signals, NOW, WINDOW));
    }

    #[test]
    fn an_unknown_os_idle_time_does_not_object() {
      let signals = AttentionSignals {
        maybe_os_idle: None,
        ..looking()
      };
      assert!(is_attentive(&signals, NOW, WINDOW));
    }

    #[test]
    fn the_input_window_is_a_setting() {
      let signals = AttentionSignals {
        last_input_at: NOW - 200_000,
        maybe_os_idle: Some(Duration::from_secs(200)),
        ..looking()
      };
      assert!(!is_attentive(&signals, NOW, WINDOW));
      assert!(is_attentive(&signals, NOW, Duration::from_secs(300)));
    }
  }

  mod reporting {
    use super::*;

    #[test]
    fn the_first_state_is_always_reported() {
      assert_eq!(AttentionReporter::default().next(false, NOW), Some(false));
      assert_eq!(AttentionReporter::default().next(true, NOW), Some(true));
    }

    #[test]
    fn becoming_attentive_is_reported_at_once() {
      let mut reporter = AttentionReporter::default();
      reporter.next(false, NOW);
      assert_eq!(reporter.next(true, NOW + 1), Some(true));
    }

    #[test]
    fn being_away_is_reported_only_once_it_lasted_a_minute() {
      let mut reporter = AttentionReporter::default();
      reporter.next(true, NOW);
      assert_eq!(reporter.next(false, NOW + 1_000), None);
      assert_eq!(reporter.next(false, NOW + 60_999), None);
      assert_eq!(reporter.next(false, NOW + 61_000), Some(false));
      assert_eq!(reporter.next(false, NOW + 600_000), None);
    }

    #[test]
    fn a_short_glance_away_is_never_reported() {
      let mut reporter = AttentionReporter::default();
      reporter.next(true, NOW);
      reporter.next(false, NOW + 1_000);
      assert_eq!(reporter.next(true, NOW + 30_000), None);
      // The away time starts again from the next glance.
      assert_eq!(reporter.next(false, NOW + 40_000), None);
      assert_eq!(reporter.next(false, NOW + 99_999), None);
      assert_eq!(reporter.next(false, NOW + 100_000), Some(false));
    }

    #[test]
    fn an_attentive_session_sends_nothing_more() {
      let mut reporter = AttentionReporter::default();
      reporter.next(true, NOW);
      for minute in 1..30 {
        assert_eq!(reporter.next(true, NOW + minute * 60_000), None);
      }
    }

    #[test]
    fn leaving_reports_away_at_once_but_only_once() {
      let mut reporter = AttentionReporter::default();
      reporter.next(true, NOW);
      assert_eq!(reporter.leave(), Some(false));
      assert_eq!(reporter.leave(), None);
      assert_eq!(reporter.last_report(), Some(false));
    }
  }

  fn looking() -> AttentionSignals {
    AttentionSignals {
      is_focused: true,
      is_visible: true,
      last_input_at: NOW - 5_000,
      maybe_os_idle: Some(Duration::from_secs(5)),
      is_screen_locked: false,
    }
  }
}
