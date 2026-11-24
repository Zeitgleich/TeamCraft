//! Counts how often parts of the top bar come and go, so a flicker that happens once in the field
//! leaves a trace in the debug log (and so in the diagnostics export and crash breadcrumbs) (#2338).
//!
//! Every top bar mount, downloads button mount and downloads visibility flip is recorded. A line
//! at `info` says what happened in the first minutes of a start; above [`CHURN_THRESHOLD`] events
//! in [`CHURN_WINDOW_MS`] one `warn` line gives the counts per kind, then stays quiet until the
//! churn stopped for a whole window.

use std::cell::RefCell;
use std::collections::VecDeque;

use crate::clock::now_ms;

/// The window events are counted in.
const CHURN_WINDOW_MS: i64 = 60_000;
/// More events than this in one window are a flicker, not a state change.
const CHURN_THRESHOLD: usize = 12;
/// Single events are logged at `info` only this often per start, so a flicker doesn't flood the log.
const LOGGED_EVENTS_PER_START: usize = 20;

/// What came or went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChurnKind {
  /// The top bar was built.
  TopBarMounted,
  /// The downloads button's component was built.
  DownloadsMounted,
  /// The downloads button became visible.
  DownloadsShown,
  /// The downloads button became hidden.
  DownloadsHidden,
}

impl ChurnKind {
  fn name(self) -> &'static str {
    match self {
      Self::TopBarMounted => "top bar mounted",
      Self::DownloadsMounted => "downloads button mounted",
      Self::DownloadsShown => "downloads button shown",
      Self::DownloadsHidden => "downloads button hidden",
    }
  }
}

/// What a recorded event leads to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChurnOutcome {
  /// Nothing worth a line.
  Quiet,
  /// A single event, logged at `info`.
  Note,
  /// Too many events in the window: the counts per kind in it.
  Flicker(Vec<(ChurnKind, usize)>),
}

/// The events of the last window, with the clock injected by the caller.
#[derive(Debug, Default)]
pub struct ChurnWatch {
  events: VecDeque<(i64, ChurnKind)>,
  is_reported: bool,
  logged: usize,
}

impl ChurnWatch {
  pub fn record(&mut self, kind: ChurnKind, at_ms: i64) -> ChurnOutcome {
    while self
      .events
      .front()
      .is_some_and(|(at, _)| at_ms.saturating_sub(*at) > CHURN_WINDOW_MS)
    {
      self.events.pop_front();
    }
    self.events.push_back((at_ms, kind));
    if self.events.len() <= CHURN_THRESHOLD {
      // Churn stopped for a whole window: the next burst is reported again.
      if self.events.len() == 1 {
        self.is_reported = false;
      }
      return self.note();
    }
    if self.is_reported {
      return ChurnOutcome::Quiet;
    }
    self.is_reported = true;
    let mut counts: Vec<(ChurnKind, usize)> = Vec::new();
    for (_, kind) in &self.events {
      match counts.iter_mut().find(|(known, _)| known == kind) {
        Some((_, count)) => *count += 1,
        None => counts.push((*kind, 1)),
      }
    }
    ChurnOutcome::Flicker(counts)
  }

  fn note(&mut self) -> ChurnOutcome {
    if self.logged >= LOGGED_EVENTS_PER_START {
      return ChurnOutcome::Quiet;
    }
    self.logged += 1;
    ChurnOutcome::Note
  }
}

thread_local! {
  static WATCH: RefCell<ChurnWatch> = RefCell::new(ChurnWatch::default());
}

/// Records one event and logs what it leads to.
pub fn record(kind: ChurnKind) {
  let at_ms = now_ms();
  let outcome = WATCH
    .try_with(|watch| {
      watch
        .try_borrow_mut()
        .ok()
        .map(|mut watch| watch.record(kind, at_ms))
    })
    .ok()
    .flatten()
    .unwrap_or(ChurnOutcome::Quiet);
  match outcome {
    ChurnOutcome::Quiet => {},
    ChurnOutcome::Note => tracing::info!(event = kind.name(), "top bar change"),
    ChurnOutcome::Flicker(counts) => {
      let summary = counts
        .iter()
        .map(|(kind, count)| format!("{}: {count}", kind.name()))
        .collect::<Vec<_>>()
        .join(", ");
      tracing::warn!(
        window_seconds = CHURN_WINDOW_MS / 1000,
        counts = %summary,
        "the top bar is flickering"
      );
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn few_events_are_noted_not_reported() {
    let mut watch = ChurnWatch::default();
    for step in 0..CHURN_THRESHOLD {
      assert_eq!(
        watch.record(ChurnKind::DownloadsShown, step as i64 * 100),
        ChurnOutcome::Note
      );
    }
  }

  #[test]
  fn a_burst_is_reported_once_with_counts() {
    let mut watch = ChurnWatch::default();
    let mut reports = Vec::new();
    for step in 0..40 {
      let kind = if step % 2 == 0 {
        ChurnKind::DownloadsShown
      } else {
        ChurnKind::DownloadsHidden
      };
      if let ChurnOutcome::Flicker(counts) = watch.record(kind, step * 100) {
        reports.push(counts);
      }
    }
    assert_eq!(reports.len(), 1);
    let total: usize = reports[0].iter().map(|(_, count)| count).sum();
    assert_eq!(total, CHURN_THRESHOLD + 1);
  }

  #[test]
  fn a_new_burst_after_a_quiet_window_is_reported_again() {
    let mut watch = ChurnWatch::default();
    for step in 0..30 {
      watch.record(ChurnKind::TopBarMounted, step * 10);
    }
    let later = 10 * CHURN_WINDOW_MS;
    let mut reports = 0;
    for step in 0..30 {
      if matches!(
        watch.record(ChurnKind::TopBarMounted, later + step * 10),
        ChurnOutcome::Flicker(_)
      ) {
        reports += 1;
      }
    }
    assert_eq!(reports, 1);
  }

  #[test]
  fn old_events_leave_the_window() {
    let mut watch = ChurnWatch::default();
    for step in 0..CHURN_THRESHOLD {
      watch.record(ChurnKind::DownloadsShown, step as i64);
    }
    let outcome = watch.record(ChurnKind::DownloadsHidden, 2 * CHURN_WINDOW_MS);
    assert!(!matches!(outcome, ChurnOutcome::Flicker(_)));
  }
}
