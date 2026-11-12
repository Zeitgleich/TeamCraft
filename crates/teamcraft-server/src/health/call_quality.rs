//! How calls went over the last minutes, from the aggregates the SFU sends with its roster
//! (`SfuRosterReport::quality`, #1666). The server sums the reports that fall into
//! `call_window_secs` and the check `call_quality` judges the sum: the 95th percentile of the
//! downlink loss, and the share of sessions that came over the WebSocket fallback.
//!
//! Counts only, no call, user, device or address. Memory is bounded by [`MAX_REPORTS`].

use std::collections::VecDeque;

use teamcraft_proto::{CALL_LOSS_BUCKETS_PERMILLE, SfuCallQuality};
use time::{Duration, OffsetDateTime};

/// Reports kept. The SFU reports every 30 s, so this covers hours, far more than any window the
/// settings allow; it only stops a misbehaving reporter from growing the list.
const MAX_REPORTS: usize = 512;
/// Fewer loss samples than this in the window say nothing about the loss (a minute of one call).
pub const MIN_LOSS_SAMPLES: u64 = 12;
/// Fewer new sessions than this say nothing about the fallback's share.
pub const MIN_SESSIONS: u64 = 5;
/// Loss above the last bucket's bound is taken as total loss when interpolating.
const TOP_BOUND_PERMILLE: f64 = 1000.0;

/// The reports of the last minutes.
#[derive(Debug, Default)]
pub struct CallQualityWindow {
  reports: VecDeque<(OffsetDateTime, SfuCallQuality)>,
}

/// The window's reports added up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CallQualitySummary {
  pub loss_samples: u64,
  /// The 95th percentile of the downlink loss, in tenths of a percent; `None` with fewer than
  /// [`MIN_LOSS_SAMPLES`].
  pub maybe_loss_p95_permille: Option<u32>,
  pub quic_sessions: u64,
  pub fallback_sessions: u64,
}

impl CallQualityWindow {
  /// Adds a report that came at `now`.
  pub fn record(&mut self, now: OffsetDateTime, quality: SfuCallQuality) {
    if self.reports.len() >= MAX_REPORTS {
      self.reports.pop_front();
    }
    self.reports.push_back((now, quality));
  }

  /// The reports of the last `window` before `now`, added up. Older ones are dropped. A report
  /// stamped in the future (the clock went back) counts as now.
  pub fn summary(&mut self, now: OffsetDateTime, window: Duration) -> CallQualitySummary {
    let since = now - window;
    self.reports.retain(|(at, _)| *at >= since);
    let mut buckets = [0_u64; CALL_LOSS_BUCKETS_PERMILLE.len() + 1];
    let mut summary = CallQualitySummary::default();
    for (_, quality) in &self.reports {
      // A report with more buckets than ours (a newer SFU) adds the rest to the top bucket.
      for (index, count) in quality.loss_buckets.iter().enumerate() {
        buckets[index.min(buckets.len() - 1)] += u64::from(*count);
      }
      summary.quic_sessions += u64::from(quality.quic_sessions);
      summary.fallback_sessions += u64::from(quality.fallback_sessions);
    }
    summary.loss_samples = buckets.iter().sum();
    if summary.loss_samples >= MIN_LOSS_SAMPLES {
      summary.maybe_loss_p95_permille = Some(quantile_permille(&buckets, 0.95));
    }
    summary
  }
}

/// The `q` quantile of samples counted per bucket of [`CALL_LOSS_BUCKETS_PERMILLE`] (and one
/// above), interpolated linearly inside its bucket, as Prometheus' `histogram_quantile` does.
fn quantile_permille(buckets: &[u64], q: f64) -> u32 {
  let total: u64 = buckets.iter().sum();
  #[allow(clippy::cast_precision_loss)]
  let rank = q * total as f64;
  let mut below = 0_u64;
  for (index, count) in buckets.iter().enumerate() {
    #[allow(clippy::cast_precision_loss)]
    let upto = (below + count) as f64;
    if *count > 0 && upto >= rank {
      let lower = if index == 0 {
        0.0
      } else {
        f64::from(CALL_LOSS_BUCKETS_PERMILLE[index - 1])
      };
      let upper = CALL_LOSS_BUCKETS_PERMILLE
        .get(index)
        .map_or(TOP_BOUND_PERMILLE, |bound| f64::from(*bound));
      #[allow(clippy::cast_precision_loss)]
      let share = (rank - below as f64) / *count as f64;
      let value = lower + (upper - lower) * share.clamp(0.0, 1.0);
      // In 0..=1000 by construction.
      #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
      return value.round() as u32;
    }
    below += count;
  }
  0
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reports_older_than_the_window_drop_out() {
    let mut window = CallQualityWindow::default();
    let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
    window.record(now - Duration::minutes(10), sessions(10, 0));
    window.record(now - Duration::minutes(2), sessions(3, 1));
    let summary = window.summary(now, Duration::minutes(5));
    assert_eq!((summary.quic_sessions, summary.fallback_sessions), (3, 1));
    assert_eq!(window.reports.len(), 1);
  }

  #[test]
  fn too_few_samples_give_no_loss() {
    let mut window = CallQualityWindow::default();
    let now = OffsetDateTime::UNIX_EPOCH;
    window.record(now, loss(&[0, 0, 0, 0, 0, 0, 0, 0, 11]));
    let summary = window.summary(now, Duration::minutes(5));
    assert_eq!(summary.loss_samples, 11);
    assert_eq!(summary.maybe_loss_p95_permille, None);
  }

  #[test]
  fn the_95th_percentile_is_interpolated_within_its_bucket() {
    // 95 samples at most 0.1 %, 5 between 5 % and 10 %: the 95th sample is the last cheap one.
    let mut window = CallQualityWindow::default();
    let now = OffsetDateTime::UNIX_EPOCH;
    window.record(now, loss(&[95, 0, 0, 0, 0, 5, 0, 0, 0]));
    assert_eq!(
      window.summary(now, Duration::minutes(5)).maybe_loss_p95_permille,
      Some(1)
    );
    // 90 cheap, 10 between 5 % and 10 %: the 95th lies half way into that bucket.
    let mut window = CallQualityWindow::default();
    window.record(now, loss(&[90, 0, 0, 0, 0, 10, 0, 0, 0]));
    assert_eq!(
      window.summary(now, Duration::minutes(5)).maybe_loss_p95_permille,
      Some(75)
    );
  }

  #[test]
  fn reports_add_up_and_unknown_buckets_count_as_the_top() {
    let mut window = CallQualityWindow::default();
    let now = OffsetDateTime::UNIX_EPOCH;
    window.record(now, loss(&[10, 0, 0, 0, 0, 0, 0, 0, 0]));
    window.record(now, loss(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 10]));
    let summary = window.summary(now, Duration::minutes(5));
    assert_eq!(summary.loss_samples, 20);
    assert_eq!(summary.maybe_loss_p95_permille, Some(950));
  }

  #[test]
  fn the_list_is_bounded() {
    let mut window = CallQualityWindow::default();
    let now = OffsetDateTime::UNIX_EPOCH;
    for _ in 0..MAX_REPORTS + 10 {
      window.record(now, sessions(1, 0));
    }
    assert_eq!(window.reports.len(), MAX_REPORTS);
  }

  fn loss(buckets: &[u32]) -> SfuCallQuality {
    SfuCallQuality {
      loss_buckets: buckets.to_vec(),
      ..SfuCallQuality::default()
    }
  }

  fn sessions(quic_sessions: u32, fallback_sessions: u32) -> SfuCallQuality {
    SfuCallQuality {
      loss_buckets: Vec::new(),
      quic_sessions,
      fallback_sessions,
      ..SfuCallQuality::default()
    }
  }
}
