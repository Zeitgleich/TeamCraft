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

/// Alerts kept for the health view, in memory and in the database: enough for a bad quarter,
/// bounded for good. Resolved ones also go after `history_days` (#1646).
pub const MAX_ALERT_RECORDS: usize = 500;

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
  /// When the alert this change is about was raised: with the check, it names the alert.
  pub raised_at: OffsetDateTime,
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

/// One alert as the history keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertRecord {
  pub check: HealthCheckId,
  pub raised_at: OffsetDateTime,
  pub maybe_resolved_at: Option<OffsetDateTime>,
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
        // A resolve whose alert left the history still tells the admins it ended.
        let raised_at = self.record(check, kind, now).unwrap_or(now);
        changes.push(AlertChange {
          check: check.clone(),
          kind,
          raised_at,
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

  /// The alert a check raised at a time, if the history still has it.
  pub fn record_of(&self, check: &HealthCheckId, raised_at: OffsetDateTime) -> Option<&AlertRecord> {
    self
      .alerts
      .iter()
      .find(|record| record.check == *check && record.raised_at == raised_at)
  }

  /// Takes the history kept from before a restart, newest first. An alert that was open goes on
  /// where it stopped: its check is in alert again, so a failure that lasts is not raised twice,
  /// and the alert ends once the check has been fine for the clear time. Returns the alerts this
  /// changed besides adding them: an old open alert of a check that raised a new one since the
  /// start is closed when the new one began.
  pub fn restore(&mut self, kept: Vec<AlertRecord>) -> Vec<AlertRecord> {
    let mut closed = Vec::new();
    for mut record in kept {
      if self.record_of(&record.check, record.raised_at).is_some() {
        continue;
      }
      if record.maybe_resolved_at.is_none() {
        match self.open_record_of(&record.check) {
          Some(newer) if newer.raised_at > record.raised_at => {
            record.maybe_resolved_at = Some(newer.raised_at);
            closed.push(record.clone());
          },
          // Two open alerts of one check: only possible with a broken history. The older wins.
          Some(_) => continue,
          None => self.reopen(&record),
        }
      }
      self.alerts.push_back(record);
    }
    self
      .alerts
      .make_contiguous()
      .sort_by_key(|record| std::cmp::Reverse(record.raised_at));
    self.alerts.truncate(MAX_ALERT_RECORDS);
    closed
  }

  /// Forgets resolved alerts that ended before `resolved_before`. Open ones stay.
  pub fn prune(&mut self, resolved_before: OffsetDateTime) {
    self.alerts.retain(|record| {
      record
        .maybe_resolved_at
        .is_none_or(|resolved_at| resolved_at >= resolved_before)
    });
  }

  fn open_record_of(&self, check: &HealthCheckId) -> Option<&AlertRecord> {
    self
      .alerts
      .iter()
      .find(|record| record.check == *check && record.maybe_resolved_at.is_none())
  }

  /// Puts a check whose alert was open before a restart back in alert.
  fn reopen(&mut self, record: &AlertRecord) {
    let track = self.tracks.entry(record.check.clone()).or_insert(Track {
      phase: Phase::Ok,
      last: Observation::new(Verdict::NoData),
    });
    track.phase = Phase::Alert {
      since: record.raised_at,
      maybe_fine_since: None,
    };
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

  /// Writes the change into the history and returns when its alert was raised. `None` for a
  /// resolve the history has no open alert for (it left the history).
  fn record(
    &mut self,
    check: &HealthCheckId,
    kind: AlertChangeKind,
    now: OffsetDateTime,
  ) -> Option<OffsetDateTime> {
    match kind {
      AlertChangeKind::Raised => {
        self.alerts.push_front(AlertRecord {
          check: check.clone(),
          raised_at: now,
          maybe_resolved_at: None,
        });
        self.alerts.truncate(MAX_ALERT_RECORDS);
        Some(now)
      },
      AlertChangeKind::Resolved => {
        let record = self
          .alerts
          .iter_mut()
          .find(|record| record.check == *check && record.maybe_resolved_at.is_none())?;
        record.maybe_resolved_at = Some(now);
        Some(record.raised_at)
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

  mod restoring {
    use super::*;

    #[test]
    fn an_alert_open_before_a_restart_is_not_raised_again_and_ends_after_the_clear_time() {
      let mut run = Run::new();
      let closed = run.tracker.restore(vec![open_record(0)]);
      assert!(closed.is_empty());
      assert_eq!(run.status(), HealthCheckStatus::Alert, "before any evaluation");
      for seconds in (STEP..HOLD * 3).step_by(STEP as usize) {
        assert_eq!(
          run.at(seconds, Verdict::Unhealthy),
          None,
          "still failing: no second raise"
        );
      }
      let fine = HOLD * 3;
      assert_eq!(run.at(fine, Verdict::Healthy), None);
      assert_eq!(
        run.at(fine + CLEAR, Verdict::Healthy),
        Some(AlertChangeKind::Resolved)
      );
      let alerts = run.tracker.alerts();
      assert_eq!(alerts.len(), 1);
      assert_eq!(alerts[0].raised_at, millis(at(0)));
      assert_eq!(alerts[0].maybe_resolved_at, Some(millis(at(fine + CLEAR))));
    }

    #[test]
    fn the_resolve_names_the_alert_raised_before_the_restart() {
      let mut run = Run::new();
      run.tracker.restore(vec![open_record(0)]);
      run.at(STEP, Verdict::Healthy);
      let changes = run.tracker.evaluate(
        at(STEP + CLEAR),
        &run.policy,
        &[(CHECK, Observation::new(Verdict::Healthy))],
      );
      assert_eq!(changes[0].raised_at, at(0));
    }

    #[test]
    fn an_old_open_alert_read_late_is_closed_by_the_one_raised_since_the_start() {
      // The database came back only after the check had raised a new alert.
      let mut run = Run::raised();
      let restart_alert = AlertRecord {
        raised_at: at(-HOLD * 5),
        ..open_record(0)
      };
      let closed = run.tracker.restore(vec![restart_alert]);
      assert_eq!(closed.len(), 1);
      assert_eq!(closed[0].maybe_resolved_at, Some(at(HOLD)));
      let alerts = run.tracker.alerts();
      assert_eq!(alerts.len(), 2);
      assert!(alerts[0].maybe_resolved_at.is_none(), "the new one stays open");
    }

    #[test]
    fn restoring_twice_keeps_one_copy_and_the_history_stays_newest_first() {
      let mut run = Run::new();
      let resolved = AlertRecord {
        check: HealthCheckId::Certificate,
        raised_at: at(-7200),
        maybe_resolved_at: Some(at(-3600)),
      };
      run.tracker.restore(vec![open_record(0), resolved.clone()]);
      run.tracker.restore(vec![open_record(0), resolved]);
      let alerts = run.tracker.alerts();
      assert_eq!(alerts.len(), 2);
      assert!(alerts[0].raised_at > alerts[1].raised_at);
    }
  }

  mod pruning {
    use super::*;

    #[test]
    fn resolved_alerts_older_than_the_history_go_and_open_ones_stay() {
      let mut tracker = AlertTracker::default();
      let old_resolved = AlertRecord {
        check: HealthCheckId::Certificate,
        raised_at: at(-7200),
        maybe_resolved_at: Some(at(-3600)),
      };
      let old_open = AlertRecord {
        raised_at: at(-7200),
        ..open_record(0)
      };
      tracker.restore(vec![old_resolved, old_open]);
      tracker.prune(at(0));
      let alerts = tracker.alerts();
      assert_eq!(alerts.len(), 1);
      assert_eq!(alerts[0].check, CHECK);
    }
  }

  // --- helpers -------------------------------------------------------------------------------

  fn open_record(seconds: i64) -> AlertRecord {
    AlertRecord {
      check: CHECK,
      raised_at: at(seconds),
      maybe_resolved_at: None,
    }
  }

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
