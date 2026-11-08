//! Calendar events that stay current when the holidays change (#1441, docs/meetings.md,
//! "Holidays").
//!
//! The calendar event of a series that skips public holidays has them as `EXDATE`s, from its
//! first occurrence to two years ahead. They change without anybody touching the meeting: a new
//! copy of the holiday feed, other holiday settings, an organizer picking a region of their own,
//! and the two years moving on. This task walks the series that skip holidays and writes the
//! event of each again whose holidays are no longer the ones it was last written with
//! ([`meetings::resync_holidays`]).
//!
//! - When: every [`INTERVAL`] (spread a little), [`FIRST_RUN`] after the start (which also
//!   catches what was asked for before a restart: nothing is lost with the memory), and
//!   [`SETTLE`] after a change asked for it, so changes that come together are one run.
//! - How much: pages of [`PAGE`] series with a pause between them, one event at a time; an event
//!   that is current costs a few queries and no write. Requests never wait for it, apart from a
//!   write of the same event that is under way.
//! - Failures: a series whose event can't be written keeps its old record, and the next run, after
//!   [`RETRY`] instead of the interval, tries it again. After [`MAX_FAILURES_IN_A_ROW`] failures
//!   in a row the run stops: the calendar or the database is down.
//! - Idempotent: a second run writes nothing. The server runs as one instance (operations.md), so
//!   one task does this.

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration as StdDuration;

use rand_core::{OsRng, RngCore};
use teamcraft_proto::{MeetingId, UserId};
use time::OffsetDateTime;
use tokio::sync::Notify;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::metrics::BackgroundJob;
use crate::services::meetings::{self, HolidayResync as Outcome};
use crate::state::AppState;

/// A full run this often: the two years ahead move on by about a week.
pub const INTERVAL: StdDuration = StdDuration::from_secs(7 * 86_400);
/// The first full run after the start.
pub const FIRST_RUN: StdDuration = StdDuration::from_secs(10 * 60);
/// The wait after a change asked for a run, for the changes that come with it.
const SETTLE: StdDuration = StdDuration::from_secs(30);
/// The next full run after one that could not write everything.
const RETRY: StdDuration = StdDuration::from_secs(3600);
/// Series read and checked in one go.
const PAGE: i64 = 50;
/// The pause between two pages.
const PAUSE: StdDuration = StdDuration::from_secs(1);
/// Failures in a row that end a run.
const MAX_FAILURES_IN_A_ROW: u32 = 5;
/// Organizers whose series wait for a run; more make it a full run.
const MAX_PENDING_ORGANIZERS: usize = 1_000;

/// The runs asked for, and the task's wake-up.
#[derive(Default)]
pub struct HolidayResync {
  pending: Mutex<Pending>,
  wake: Notify,
}

#[derive(Default)]
struct Pending {
  is_everything: bool,
  organizers: BTreeSet<UserId>,
}

/// Which series a run looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
  Everything,
  /// The series of these organizers.
  Organizers(Vec<UserId>),
}

/// What a run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
  /// Series looked at.
  pub checked: u32,
  /// Events written again.
  pub rewritten: u32,
  /// Series whose event could not be checked or written.
  pub failed: u32,
  /// The run went through all series of its scope.
  pub is_complete: bool,
}

impl HolidayResync {
  /// The holidays of the instance changed (settings, a new copy of the feed): all series.
  pub fn holidays_changed(&self) {
    self.lock().is_everything = true;
    self.wake.notify_one();
  }

  /// `organizer` picked another region: their series.
  pub fn region_changed(&self, organizer: UserId) {
    {
      let mut pending = self.lock();
      if pending.organizers.len() < MAX_PENDING_ORGANIZERS {
        pending.organizers.insert(organizer);
      } else {
        pending.is_everything = true;
      }
    }
    self.wake.notify_one();
  }

  /// The run asked for, if any; asking again starts anew.
  fn take(&self) -> Option<Scope> {
    let mut pending = self.lock();
    let taken = std::mem::take(&mut *pending);
    if taken.is_everything {
      Some(Scope::Everything)
    } else if taken.organizers.is_empty() {
      None
    } else {
      Some(Scope::Organizers(taken.organizers.into_iter().collect()))
    }
  }

  fn lock(&self) -> MutexGuard<'_, Pending> {
    self.pending.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

impl Report {
  fn is_clean(&self) -> bool {
    self.is_complete && self.failed == 0
  }
}

/// Runs in the background until the process ends.
pub fn spawn(state: AppState) {
  tokio::spawn(async move {
    let mut next_full = Instant::now() + spread(FIRST_RUN, OsRng.next_u32());
    loop {
      tokio::select! {
        () = tokio::time::sleep_until(next_full) => state.holiday_resync.lock().is_everything = true,
        () = state.holiday_resync.wake.notified() => tokio::time::sleep(SETTLE).await,
      }
      let Some(scope) = state.holiday_resync.take() else {
        continue;
      };
      let report = run(&state, &scope, PAUSE).await;
      if report.rewritten > 0 || report.failed > 0 || !report.is_complete {
        info!(?scope, ?report, "calendar events checked against the holidays");
      } else {
        debug!(?scope, ?report, "calendar events checked against the holidays");
      }
      if !report.is_clean() {
        next_full = Instant::now() + spread(RETRY, OsRng.next_u32());
      } else if scope == Scope::Everything {
        state
          .metrics
          .job_succeeded(BackgroundJob::HolidayResync, OffsetDateTime::now_utc());
        next_full = Instant::now() + spread(INTERVAL, OsRng.next_u32());
      }
    }
  });
}

/// Checks the series of `scope` page by page, with `pause` between pages, and writes the events
/// whose holidays changed. Without a calendar there is nothing to do.
pub async fn run(state: &AppState, scope: &Scope, pause: StdDuration) -> Report {
  let mut report = Report::default();
  let has_calendar = state
    .maybe_meetings
    .as_deref()
    .is_some_and(|service| service.calendar().is_some());
  if !has_calendar {
    report.is_complete = true;
    return report;
  }
  let maybe_organizers = match scope {
    Scope::Everything => None,
    Scope::Organizers(organizers) => Some(organizers.as_slice()),
  };
  let mut maybe_after: Option<MeetingId> = None;
  let mut failures_in_a_row = 0;
  loop {
    let page = match state
      .database
      .series_skipping_holidays(maybe_after, maybe_organizers, PAGE)
      .await
    {
      Ok(page) => page,
      Err(error) => {
        warn!(%error, "reading the series that skip holidays failed");
        return report;
      },
    };
    for id in &page {
      report.checked += 1;
      match meetings::resync_holidays(state, *id).await {
        Ok(outcome) => {
          failures_in_a_row = 0;
          if outcome == Outcome::Rewritten {
            report.rewritten += 1;
          }
        },
        Err(error) => {
          report.failed += 1;
          failures_in_a_row += 1;
          warn!(%error, meeting_id = %id, "writing the calendar event with the current holidays failed");
          if failures_in_a_row >= MAX_FAILURES_IN_A_ROW {
            return report;
          }
        },
      }
    }
    if page.len() < usize::try_from(PAGE).unwrap_or(usize::MAX) {
      report.is_complete = true;
      return report;
    }
    maybe_after = page.last().copied();
    if !pause.is_zero() {
      tokio::time::sleep(pause).await;
    }
  }
}

/// `base` and up to a tenth more, so runs of instances started together drift apart.
fn spread(base: StdDuration, random: u32) -> StdDuration {
  base + base.mul_f64(f64::from(random) / f64::from(u32::MAX) / 10.0)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  #[test]
  fn requests_add_up_and_a_full_run_covers_the_organizers() {
    let resync = HolidayResync::default();
    assert_eq!(resync.take(), None);
    resync.region_changed(user(7));
    resync.region_changed(user(5));
    resync.region_changed(user(7));
    assert_eq!(resync.take(), Some(Scope::Organizers(vec![user(5), user(7)])));
    assert_eq!(resync.take(), None);
    resync.region_changed(user(5));
    resync.holidays_changed();
    assert_eq!(resync.take(), Some(Scope::Everything));
    assert_eq!(resync.take(), None);
  }

  #[test]
  fn too_many_organizers_make_a_full_run() {
    let resync = HolidayResync::default();
    for raw in 0..=i64::try_from(MAX_PENDING_ORGANIZERS).unwrap() {
      resync.region_changed(user(raw + 1));
    }
    assert_eq!(resync.take(), Some(Scope::Everything));
  }

  #[test]
  fn runs_are_spread_by_at_most_a_tenth() {
    let week = INTERVAL;
    assert_eq!(spread(week, 0), week);
    let most = spread(week, u32::MAX);
    assert!(most > week && most <= week + week / 10 + StdDuration::from_secs(1));
  }
}
