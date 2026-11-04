//! When a check that fails becomes an alert, and when the alert ends. Pure: the time is an argument,
//! so the tests run without a clock and without sleeping.
//!
//! The rules that keep it from flapping (docs/operations.md, "Health and alerts"):
//! - a check raises an alert only after failing for the hold time without a break; one good answer
//!   in between starts the hold again;
//! - an alert ends only after the check has been fine for the clear time, and one failure in
//!   between starts that again;
//! - a gap between two evaluations (the process was frozen, or the clock jumped) forgets what was
//!   counted, so a pause cannot add up to a hold the checks never saw.

use std::collections::{HashMap, VecDeque};

use teamcraft_proto::{
  HealthAlertResponse, HealthCheckId, HealthCheckResponse, HealthCheckStatus, UnixMillis,
};
use time::{Duration, OffsetDateTime};

/// Alerts kept for the health view: enough for a bad week, bounded for good.
const MAX_ALERT_RECORDS: usize = 50;

/// What one evaluation says about one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
  Healthy,
  Unhealthy,
  /// Nothing to judge by yet (just started, nothing reported): changes nothing.
  NoData,
  /// Not set up on this instance.
  NotConfigured,
}

/// A verdict with the number it was reached by, where there is one (see
/// [`HealthCheckResponse::maybe_value`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
  pub verdict: Verdict,
  pub maybe_value: Option<i64>,
}

/// The settings an evaluation follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlertPolicy {
  pub is_enabled: bool,
  pub hold: Duration,
  pub clear: Duration,
  /// Longest time between two evaluations that still counts as continuous.
  pub max_gap: Duration,
}

/// An alert was raised or resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertChange {
  pub check: HealthCheckId,
  pub kind: AlertChangeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertChangeKind {
  Raised,
  Resolved,
}

/// Every check's state, from the evaluations so far.
#[derive(Debug, Default)]
pub struct AlertTracker {
  tracks: HashMap<HealthCheckId, Track>,
  /// Newest first.
  alerts: VecDeque<AlertRecord>,
  maybe_last_evaluation: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
struct Track {
  phase: Phase,
  last: Observation,
}

#[derive(Debug, Clone, Copy)]
enum Phase {
  Ok,
  Failing {
    since: OffsetDateTime,
  },
  Alert {
    since: OffsetDateTime,
    /// Set while the check is fine again, until the clear time has passed.
    maybe_fine_since: Option<OffsetDateTime>,
  },
}

#[derive(Debug, Clone)]
struct AlertRecord {
  check: HealthCheckId,
  raised_at: OffsetDateTime,
  maybe_resolved_at: Option<OffsetDateTime>,
}

impl AlertTracker {
  /// Takes one round of observations and returns the alerts it raised or resolved.
  pub fn evaluate(
    &mut self,
    now: OffsetDateTime,
    policy: &AlertPolicy,
    observations: &[(HealthCheckId, Observation)],
  ) -> Vec<AlertChange> {
    self.forget_counting_across_a_gap(now, policy);
    self.maybe_last_evaluation = Some(now);
    let mut changes = Vec::new();
    for (check, observation) in observations {
      let track = self.tracks.entry(check.clone()).or_insert(Track {
        phase: Phase::Ok,
        last: *observation,
      });
      track.last = *observation;
      let (phase, maybe_change) = step(track.phase, observation.verdict, now, policy);
      track.phase = phase;
      if let Some(kind) = maybe_change {
        self.record(check, kind, now);
        changes.push(AlertChange {
          check: check.clone(),
          kind,
        });
      }
    }
    changes
  }

  /// The checks in the order of `order`, as the API shows them. A check not evaluated yet has no
  /// data.
  pub fn checks(&self, order: &[HealthCheckId]) -> Vec<HealthCheckResponse> {
    order
      .iter()
      .map(|check| match self.tracks.get(check) {
        None => HealthCheckResponse {
          check: check.clone(),
          status: HealthCheckStatus::NoData,
          maybe_failing_since: None,
          maybe_value: None,
        },
        Some(track) => HealthCheckResponse {
          check: check.clone(),
          status: status_of(track),
          maybe_failing_since: match track.phase {
            Phase::Ok => None,
            Phase::Failing { since } | Phase::Alert { since, .. } => Some(millis(since)),
          },
          maybe_value: track.last.maybe_value,
        },
      })
      .collect()
  }

  /// The alerts raised since the start, newest first.
  pub fn alerts(&self) -> Vec<HealthAlertResponse> {
    self
      .alerts
      .iter()
      .map(|record| HealthAlertResponse {
        check: record.check.clone(),
        raised_at: millis(record.raised_at),
        maybe_resolved_at: record.maybe_resolved_at.map(millis),
      })
      .collect()
  }

  pub fn maybe_last_evaluation(&self) -> Option<OffsetDateTime> {
    self.maybe_last_evaluation
  }

  fn forget_counting_across_a_gap(&mut self, now: OffsetDateTime, policy: &AlertPolicy) {
    let Some(last) = self.maybe_last_evaluation else {
      return;
    };
    if now - last <= policy.max_gap {
      return;
    }
    for track in self.tracks.values_mut() {
      track.phase = match track.phase {
        Phase::Failing { .. } => Phase::Ok,
        Phase::Alert { since, .. } => Phase::Alert {
          since,
          maybe_fine_since: None,
        },
        Phase::Ok => Phase::Ok,
      };
    }
  }

  fn record(&mut self, check: &HealthCheckId, kind: AlertChangeKind, now: OffsetDateTime) {
    match kind {
      AlertChangeKind::Raised => {
        self.alerts.push_front(AlertRecord {
          check: check.clone(),
          raised_at: now,
          maybe_resolved_at: None,
        });
        self.alerts.truncate(MAX_ALERT_RECORDS);
      },
      AlertChangeKind::Resolved => {
        if let Some(record) = self
          .alerts
          .iter_mut()
          .find(|record| record.check == *check && record.maybe_resolved_at.is_none())
        {
          record.maybe_resolved_at = Some(now);
        }
      },
    }
  }
}

/// One check's move for one verdict.
fn step(
  phase: Phase,
  verdict: Verdict,
  now: OffsetDateTime,
  policy: &AlertPolicy,
) -> (Phase, Option<AlertChangeKind>) {
  match (verdict, phase) {
    // Switched off, or no longer set up: an open alert has nothing left to say.
    (Verdict::NotConfigured, Phase::Alert { .. }) => (Phase::Ok, Some(AlertChangeKind::Resolved)),
    (Verdict::NotConfigured, _) => (Phase::Ok, None),
    (Verdict::NoData, phase) => (phase, None),
    (_, Phase::Alert { .. }) if !policy.is_enabled => (Phase::Ok, Some(AlertChangeKind::Resolved)),
    (Verdict::Healthy, Phase::Failing { .. } | Phase::Ok) => (Phase::Ok, None),
    (
      Verdict::Healthy,
      Phase::Alert {
        since,
        maybe_fine_since,
      },
    ) => {
      let fine_since = maybe_fine_since.unwrap_or(now);
      if now - fine_since >= policy.clear {
        (Phase::Ok, Some(AlertChangeKind::Resolved))
      } else {
        (
          Phase::Alert {
            since,
            maybe_fine_since: Some(fine_since),
          },
          None,
        )
      }
    },
    (Verdict::Unhealthy, Phase::Ok) => (Phase::Failing { since: now }, None),
    (Verdict::Unhealthy, Phase::Failing { since }) => {
      if policy.is_enabled && now - since >= policy.hold {
        (
          Phase::Alert {
            since,
            maybe_fine_since: None,
          },
          Some(AlertChangeKind::Raised),
        )
      } else {
        (Phase::Failing { since }, None)
      }
    },
    (Verdict::Unhealthy, Phase::Alert { since, .. }) => (
      Phase::Alert {
        since,
        maybe_fine_since: None,
      },
      None,
    ),
  }
}

fn status_of(track: &Track) -> HealthCheckStatus {
  match (track.last.verdict, track.phase) {
    (Verdict::NotConfigured, _) => HealthCheckStatus::NotConfigured,
    (_, Phase::Alert { .. }) => HealthCheckStatus::Alert,
    (_, Phase::Failing { .. }) => HealthCheckStatus::Failing,
    (Verdict::NoData, Phase::Ok) => HealthCheckStatus::NoData,
    (_, Phase::Ok) => HealthCheckStatus::Ok,
  }
}

pub(super) fn millis(time: OffsetDateTime) -> UnixMillis {
  UnixMillis(i64::try_from(time.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX))
}

impl Observation {
  pub fn new(verdict: Verdict) -> Self {
    Self {
      verdict,
      maybe_value: None,
    }
  }

  pub fn with_value(verdict: Verdict, value: i64) -> Self {
    Self {
      verdict,
      maybe_value: Some(value),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const CHECK: HealthCheckId = HealthCheckId::Backup;

  mod raising {
    use super::*;

    #[test]
    fn a_failure_is_an_alert_only_after_the_hold_time() {
      let mut run = Run::new();
      assert_eq!(run.at(0, Verdict::Unhealthy), None);
      assert_eq!(run.at(HOLD - 1, Verdict::Unhealthy), None);
      assert_eq!(run.status(), HealthCheckStatus::Failing);
      assert_eq!(run.at(HOLD, Verdict::Unhealthy), Some(AlertChangeKind::Raised));
      assert_eq!(run.status(), HealthCheckStatus::Alert);
      assert_eq!(
        run.at(HOLD + STEP, Verdict::Unhealthy),
        None,
        "raised once, not on every round"
      );
    }

    #[test]
    fn one_good_answer_starts_the_hold_again() {
      let mut run = Run::new();
      run.at(0, Verdict::Unhealthy);
      run.at(HOLD - STEP, Verdict::Healthy);
      assert_eq!(run.status(), HealthCheckStatus::Ok);
      run.at(HOLD, Verdict::Unhealthy);
      assert_eq!(run.at(HOLD + HOLD - STEP, Verdict::Unhealthy), None);
      assert_eq!(
        run.at(HOLD + HOLD, Verdict::Unhealthy),
        Some(AlertChangeKind::Raised)
      );
    }

    #[test]
    fn no_data_neither_raises_nor_cancels() {
      let mut run = Run::new();
      run.at(0, Verdict::Unhealthy);
      run.at(HOLD / 2, Verdict::NoData);
      assert_eq!(run.status(), HealthCheckStatus::Failing);
      assert_eq!(run.at(HOLD, Verdict::Unhealthy), Some(AlertChangeKind::Raised));
    }

    #[test]
    fn a_check_without_data_says_so_and_a_missing_setup_says_that() {
      let mut run = Run::new();
      run.at(0, Verdict::NoData);
      assert_eq!(run.status(), HealthCheckStatus::NoData);
      run.at(STEP, Verdict::NotConfigured);
      assert_eq!(run.status(), HealthCheckStatus::NotConfigured);
    }

    #[test]
    fn with_alerts_off_nothing_is_raised_but_the_failure_still_shows() {
      let mut run = Run::new();
      run.policy.is_enabled = false;
      run.at(0, Verdict::Unhealthy);
      assert_eq!(run.at(HOLD * 3, Verdict::Unhealthy), None);
      assert_eq!(run.status(), HealthCheckStatus::Failing);
    }

    #[test]
    fn a_pause_between_evaluations_does_not_add_up_to_a_hold() {
      let mut run = Run::new();
      run.policy.max_gap = Duration::seconds(STEP * 3);
      run.at(0, Verdict::Unhealthy);
      // The process was frozen for an hour: the check was not seen failing in between.
      assert_eq!(run.at(3600, Verdict::Unhealthy), None);
      assert_eq!(run.status(), HealthCheckStatus::Failing);
      // From then on it is counted again, round after round.
      for seconds in (STEP..HOLD).step_by(STEP as usize) {
        assert_eq!(run.at(3600 + seconds, Verdict::Unhealthy), None);
      }
      assert_eq!(
        run.at(3600 + HOLD, Verdict::Unhealthy),
        Some(AlertChangeKind::Raised)
      );
    }
  }

  mod resolving {
    use super::*;

    #[test]
    fn an_alert_ends_only_after_the_clear_time() {
      let mut run = Run::raised();
      let fine = HOLD + STEP;
      assert_eq!(run.at(fine, Verdict::Healthy), None);
      assert_eq!(run.status(), HealthCheckStatus::Alert);
      assert_eq!(run.at(fine + CLEAR - 1, Verdict::Healthy), None);
      assert_eq!(
        run.at(fine + CLEAR, Verdict::Healthy),
        Some(AlertChangeKind::Resolved)
      );
      assert_eq!(run.status(), HealthCheckStatus::Ok);
    }

    #[test]
    fn a_failure_in_between_starts_the_clear_time_again() {
      let mut run = Run::raised();
      let fine = HOLD + STEP;
      run.at(fine, Verdict::Healthy);
      run.at(fine + CLEAR - STEP, Verdict::Unhealthy);
      assert_eq!(run.at(fine + CLEAR, Verdict::Healthy), None);
      assert_eq!(run.at(fine + CLEAR + STEP, Verdict::Healthy), None);
      assert_eq!(
        run.at(fine + CLEAR + CLEAR, Verdict::Healthy),
        Some(AlertChangeKind::Resolved)
      );
    }

    #[test]
    fn a_pause_does_not_resolve_an_alert() {
      let mut run = Run::raised();
      run.policy.max_gap = Duration::seconds(STEP * 3);
      run.at(HOLD + STEP, Verdict::Healthy);
      assert_eq!(run.at(HOLD + STEP + 3600, Verdict::Healthy), None);
      assert_eq!(run.status(), HealthCheckStatus::Alert);
    }

    #[test]
    fn switching_alerts_off_or_the_check_away_resolves_what_is_open() {
      let mut run = Run::raised();
      run.policy.is_enabled = false;
      assert_eq!(
        run.at(HOLD + STEP, Verdict::Unhealthy),
        Some(AlertChangeKind::Resolved)
      );

      let mut run = Run::raised();
      assert_eq!(
        run.at(HOLD + STEP, Verdict::NotConfigured),
        Some(AlertChangeKind::Resolved)
      );
    }
  }

  mod history {
    use super::*;

    #[test]
    fn the_list_has_the_newest_first_with_the_end_of_resolved_ones() {
      let mut run = Run::raised();
      run.at(HOLD + STEP, Verdict::Healthy);
      run.at(HOLD + STEP + CLEAR, Verdict::Healthy);
      run.at(HOLD + STEP + CLEAR + STEP, Verdict::Unhealthy);
      run.at(HOLD + STEP + CLEAR + STEP + HOLD, Verdict::Unhealthy);
      let alerts = run.tracker.alerts();
      assert_eq!(alerts.len(), 2);
      assert!(alerts[0].maybe_resolved_at.is_none());
      assert!(alerts[1].maybe_resolved_at.is_some());
      assert!(alerts[0].raised_at > alerts[1].raised_at);
    }

    #[test]
    fn the_list_is_bounded() {
      let mut tracker = AlertTracker::default();
      let mut time = at(0);
      let policy = policy();
      for _ in 0..MAX_ALERT_RECORDS * 2 {
        for verdict in [
          Verdict::Unhealthy,
          Verdict::Unhealthy,
          Verdict::Healthy,
          Verdict::Healthy,
        ] {
          let step = if verdict == Verdict::Unhealthy {
            HOLD
          } else {
            CLEAR
          };
          time += Duration::seconds(step);
          tracker.evaluate(time, &policy, &[(CHECK, Observation::new(verdict))]);
        }
      }
      assert!(tracker.alerts().len() <= MAX_ALERT_RECORDS);
    }
  }

  // --- helpers -------------------------------------------------------------------------------

  const HOLD: i64 = 600;
  const CLEAR: i64 = 300;
  const STEP: i64 = 30;

  /// One check, evaluated at chosen seconds.
  struct Run {
    tracker: AlertTracker,
    policy: AlertPolicy,
  }

  impl Run {
    fn new() -> Self {
      Self {
        tracker: AlertTracker::default(),
        policy: policy(),
      }
    }

    /// An alert raised at `HOLD`.
    fn raised() -> Self {
      let mut run = Self::new();
      run.at(0, Verdict::Unhealthy);
      run.at(HOLD, Verdict::Unhealthy);
      run
    }

    fn at(&mut self, seconds: i64, verdict: Verdict) -> Option<AlertChangeKind> {
      let changes = self
        .tracker
        .evaluate(at(seconds), &self.policy, &[(CHECK, Observation::new(verdict))]);
      assert!(changes.len() <= 1);
      changes.first().map(|change| change.kind)
    }

    fn status(&self) -> HealthCheckStatus {
      self.tracker.checks(&[CHECK])[0].status.clone()
    }
  }

  fn policy() -> AlertPolicy {
    AlertPolicy {
      is_enabled: true,
      hold: Duration::seconds(HOLD),
      clear: Duration::seconds(CLEAR),
      max_gap: Duration::days(1),
    }
  }

  fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_800_000_000 + seconds)
  }
}
