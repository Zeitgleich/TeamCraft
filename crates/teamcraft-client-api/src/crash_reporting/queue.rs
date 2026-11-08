//! Reports waiting to be sent: bounded in count and bytes, kept across restarts by the shell
//! (it stores the serialized queue), retried with a backoff, dropped when too old.

use std::collections::VecDeque;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::event::EventId;

/// First wait after a failed send; doubles with every failure.
const FIRST_BACKOFF: Duration = Duration::from_secs(30);
/// Longest wait between two tries of one report.
const MAX_BACKOFF: Duration = Duration::from_secs(6 * 60 * 60);

/// Bounds of the queue. A slow or dead backend can't make it grow: the oldest reports drop out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrashQueueLimits {
  pub max_reports: usize,
  pub max_bytes: usize,
  /// Tries per report; after the last failed one it is dropped.
  pub max_attempts: u32,
  /// A report older than this is dropped unsent.
  pub max_age: Duration,
}

/// The queue, oldest report first. Serializable so shells keep it across restarts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CrashQueue {
  reports: VecDeque<QueuedCrashReport>,
  /// The backend asked to wait (`Retry-After`): nothing is sent before this.
  maybe_paused_until_ms: Option<i64>,
}

/// One envelope waiting to be sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedCrashReport {
  pub event_id: EventId,
  pub created_at_ms: i64,
  /// The scrubbed envelope, ready to send.
  pub envelope: String,
  pub attempts: u32,
  pub next_attempt_at_ms: i64,
}

impl Default for CrashQueueLimits {
  fn default() -> Self {
    Self {
      max_reports: 20,
      max_bytes: 512 * 1024,
      max_attempts: 8,
      max_age: Duration::from_secs(7 * 24 * 60 * 60),
    }
  }
}

impl CrashQueue {
  /// Adds a report, dropping the oldest until the queue fits its limits. A report larger than
  /// the whole byte limit is not queued.
  pub fn push(&mut self, report: QueuedCrashReport, limits: &CrashQueueLimits) -> bool {
    if report.envelope.len() > limits.max_bytes || limits.max_reports == 0 {
      return false;
    }
    self.reports.retain(|queued| queued.event_id != report.event_id);
    self.reports.push_back(report);
    while self.reports.len() > limits.max_reports || self.total_bytes() > limits.max_bytes {
      self.reports.pop_front();
    }
    true
  }

  pub fn len(&self) -> usize {
    self.reports.len()
  }

  pub fn is_empty(&self) -> bool {
    self.reports.is_empty()
  }

  /// The oldest report due at `now_ms`, unless the backend asked to wait.
  pub fn next_due(&self, now_ms: i64) -> Option<&QueuedCrashReport> {
    if self.maybe_paused_until_ms.is_some_and(|until| now_ms < until) {
      return None;
    }
    self
      .reports
      .iter()
      .find(|report| report.next_attempt_at_ms <= now_ms)
  }

  /// Sent, or refused for good: gone.
  pub fn remove(&mut self, event_id: EventId) {
    self.reports.retain(|report| report.event_id != event_id);
  }

  /// A failed try: wait longer next time; drop it after the last try.
  pub fn record_failure(&mut self, event_id: EventId, now_ms: i64, limits: &CrashQueueLimits) {
    let Some(report) = self.reports.iter_mut().find(|report| report.event_id == event_id) else {
      return;
    };
    report.attempts += 1;
    if report.attempts >= limits.max_attempts {
      self.remove(event_id);
      return;
    }
    let backoff = FIRST_BACKOFF
      .saturating_mul(1 << report.attempts.min(16))
      .min(MAX_BACKOFF);
    report.next_attempt_at_ms = now_ms.saturating_add(duration_ms(backoff));
  }

  /// The backend asked to wait: no report is sent before `now_ms + retry_after`.
  pub fn pause(&mut self, now_ms: i64, retry_after: Duration) {
    self.maybe_paused_until_ms = Some(now_ms.saturating_add(duration_ms(retry_after.min(MAX_BACKOFF))));
  }

  /// Drops reports older than the limit.
  pub fn prune(&mut self, now_ms: i64, limits: &CrashQueueLimits) {
    let oldest = now_ms.saturating_sub(duration_ms(limits.max_age));
    self.reports.retain(|report| report.created_at_ms >= oldest);
  }

  /// Drops everything (reports turned off, or the user opted out).
  pub fn clear(&mut self) {
    self.reports.clear();
    self.maybe_paused_until_ms = None;
  }

  fn total_bytes(&self) -> usize {
    self.reports.iter().map(|report| report.envelope.len()).sum()
  }
}

fn duration_ms(duration: Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn drops_the_oldest_beyond_the_count() {
    let limits = CrashQueueLimits {
      max_reports: 2,
      ..CrashQueueLimits::default()
    };
    let mut queue = CrashQueue::default();
    for index in 0..3 {
      assert!(queue.push(report(index, 10), &limits));
    }
    assert_eq!(queue.len(), 2);
    assert_eq!(queue.next_due(0).unwrap().event_id, id(1));
  }

  #[test]
  fn drops_the_oldest_beyond_the_bytes_and_refuses_a_giant() {
    let limits = CrashQueueLimits {
      max_bytes: 100,
      ..CrashQueueLimits::default()
    };
    let mut queue = CrashQueue::default();
    queue.push(report(0, 60), &limits);
    queue.push(report(1, 60), &limits);
    assert_eq!(queue.len(), 1);
    assert!(!queue.push(report(2, 101), &limits));
    assert_eq!(queue.next_due(0).unwrap().event_id, id(1));
  }

  #[test]
  fn backs_off_and_gives_up() {
    let limits = CrashQueueLimits {
      max_attempts: 3,
      ..CrashQueueLimits::default()
    };
    let mut queue = CrashQueue::default();
    queue.push(report(0, 10), &limits);
    queue.record_failure(id(0), 1_000, &limits);
    assert!(queue.next_due(1_000).is_none());
    assert!(queue.next_due(1_000 + 60_000).is_some());
    queue.record_failure(id(0), 61_000, &limits);
    assert!(queue.next_due(61_000 + 119_999).is_none());
    assert!(queue.next_due(61_000 + 120_000).is_some());
    queue.record_failure(id(0), 200_000, &limits);
    assert!(queue.is_empty());
  }

  #[test]
  fn waits_while_paused() {
    let mut queue = CrashQueue::default();
    queue.push(report(0, 10), &CrashQueueLimits::default());
    queue.pause(0, Duration::from_secs(60));
    assert!(queue.next_due(59_999).is_none());
    assert!(queue.next_due(60_000).is_some());
  }

  #[test]
  fn prunes_old_reports_and_survives_a_restart() {
    let limits = CrashQueueLimits::default();
    let mut queue = CrashQueue::default();
    queue.push(report(0, 10), &limits);
    let json = serde_json::to_string(&queue).unwrap();
    let mut restored: CrashQueue = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, queue);
    restored.prune(duration_ms(limits.max_age) + 1, &limits);
    assert!(restored.is_empty());
  }

  fn id(index: u8) -> EventId {
    EventId::from_random_bytes([index; 16])
  }

  fn report(index: u8, bytes: usize) -> QueuedCrashReport {
    QueuedCrashReport {
      event_id: id(index),
      created_at_ms: 0,
      envelope: "x".repeat(bytes),
      attempts: 0,
      next_attempt_at_ms: 0,
    }
  }
}
