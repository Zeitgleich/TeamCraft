//! Which of a mobile app's lifecycle and input signals become device activity reports
//! (docs/gateway.md, "Presence"; issue #472). The server ends an away timeout on `active`, so the app
//! reports it when it comes to the foreground and on input, and `idle` when it goes to the
//! background. Input repeats `active` at most every [`ACTIVE_REPEAT_INTERVAL`], so touching the
//! screen all day costs one small frame per half minute.
//!
//! Times are monotonic ([`Instant`]): a wall clock set back would otherwise silence input reports
//! until it caught up again.

use std::time::{Duration, Instant};

use teamcraft_proto::DeviceActivity;

/// How often input repeats `active` while the app is in front: half the shortest away timeout the
/// workspace policy allows (a minute), as on web and desktop, so a user who only scrolls never times
/// out, and the first touch after an automatic away ends it at once.
pub const ACTIVE_REPEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Decides which signals reach the server; holds what was reported last.
#[derive(Clone, Copy, Debug, Default)]
pub struct ActivityReports {
  maybe_last: Option<LastReport>,
}

/// What the app tells the core about its use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppUse {
  /// The app came to the foreground (opened, resumed, unlocked).
  Foreground,
  /// The user touched the screen or pressed a key.
  Interaction,
  /// The app went to the background.
  Background,
}

/// The last report and when it went out.
#[derive(Clone, Copy, Debug)]
struct LastReport {
  is_idle: bool,
  at: Instant,
}

impl ActivityReports {
  /// The report `used` calls for at `now`, if any. Foreground and background always report (they
  /// change what the server should know); input reports only when the device was idle or the last
  /// `active` is [`ACTIVE_REPEAT_INTERVAL`] old.
  pub fn next(&mut self, used: AppUse, now: Instant) -> Option<DeviceActivity> {
    let is_idle = match used {
      AppUse::Foreground => false,
      AppUse::Background => true,
      AppUse::Interaction => {
        if self.is_active_report_fresh(now) {
          return None;
        }
        false
      },
    };
    self.maybe_last = Some(LastReport { is_idle, at: now });
    Some(if is_idle {
      DeviceActivity::Idle
    } else {
      DeviceActivity::Active
    })
  }

  /// Whether `active` went out less than [`ACTIVE_REPEAT_INTERVAL`] before `now`.
  fn is_active_report_fresh(&self, now: Instant) -> bool {
    self
      .maybe_last
      .is_some_and(|last| !last.is_idle && now.saturating_duration_since(last.at) < ACTIVE_REPEAT_INTERVAL)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn foreground_and_background_always_report() {
    let start = Instant::now();
    let mut reports = ActivityReports::default();
    assert_eq!(
      reports.next(AppUse::Foreground, start),
      Some(DeviceActivity::Active)
    );
    assert_eq!(
      reports.next(AppUse::Foreground, start + secs(1)),
      Some(DeviceActivity::Active)
    );
    assert_eq!(
      reports.next(AppUse::Background, start + secs(2)),
      Some(DeviceActivity::Idle)
    );
    assert_eq!(
      reports.next(AppUse::Background, start + secs(3)),
      Some(DeviceActivity::Idle)
    );
  }

  #[test]
  fn input_repeats_active_every_thirty_seconds_at_most() {
    let start = Instant::now();
    let mut reports = ActivityReports::default();
    reports.next(AppUse::Foreground, start);
    assert_eq!(reports.next(AppUse::Interaction, start + secs(1)), None);
    assert_eq!(reports.next(AppUse::Interaction, start + secs(29)), None);
    assert_eq!(
      reports.next(AppUse::Interaction, start + secs(30)),
      Some(DeviceActivity::Active)
    );
    // The interval counts from the last report, not from the first input after it.
    assert_eq!(reports.next(AppUse::Interaction, start + secs(59)), None);
    assert_eq!(
      reports.next(AppUse::Interaction, start + secs(60)),
      Some(DeviceActivity::Active)
    );
  }

  #[test]
  fn input_after_idle_or_before_any_report_reports_active_at_once() {
    let start = Instant::now();
    let mut reports = ActivityReports::default();
    assert_eq!(
      reports.next(AppUse::Interaction, start),
      Some(DeviceActivity::Active)
    );
    reports.next(AppUse::Background, start + secs(1));
    assert_eq!(
      reports.next(AppUse::Interaction, start + secs(2)),
      Some(DeviceActivity::Active)
    );
    assert_eq!(reports.next(AppUse::Interaction, start + secs(3)), None);
  }

  #[test]
  fn coming_back_resets_the_input_interval() {
    let start = Instant::now();
    let mut reports = ActivityReports::default();
    reports.next(AppUse::Interaction, start);
    reports.next(AppUse::Foreground, start + secs(20));
    assert_eq!(reports.next(AppUse::Interaction, start + secs(31)), None);
    assert_eq!(
      reports.next(AppUse::Interaction, start + secs(50)),
      Some(DeviceActivity::Active)
    );
  }

  #[test]
  fn an_earlier_instant_than_the_last_report_counts_as_fresh() {
    let start = Instant::now() + secs(60);
    let mut reports = ActivityReports::default();
    reports.next(AppUse::Foreground, start);
    assert_eq!(reports.next(AppUse::Interaction, start - secs(10)), None);
  }

  fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
  }
}
