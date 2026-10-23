//! What the server remembers about pushes it sent (notifications.md, "Mobile push delivery"):
//! which devices show a notification for which conversation (to withdraw it when the user reads
//! elsewhere), which devices ring for a call (to stop them when the ring ends), delivery counts per
//! device for the admin view, and when a device last got a test push.
//!
//! In memory and per process, like the gateway hub: after a restart nothing is withdrawn for pushes
//! sent before it, which costs a stale notification, never a missing one.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use teamcraft_proto::{
  CallId, ConversationId, DeviceId, MessageId, PushDeliveryCounts, ThreadId, UnixMillis, UserId,
};
use tokio::time::Instant;

/// Notification entries kept at most; beyond that the oldest are forgotten.
const MAX_SHOWN_ENTRIES: usize = 100_000;
/// Rings kept at most (each is short-lived; this only bounds a flood).
const MAX_RINGS: usize = 10_000;
/// Devices whose counts are kept at most.
const MAX_HEALTH_DEVICES: usize = 100_000;

/// The tracker shared by the routing and the delivery.
#[derive(Default)]
pub struct PushTracker {
  inner: Mutex<Inner>,
}

/// What happened to one push, for the counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryEvent {
  Sent,
  Failed,
  Retried,
  Dismissed,
  SuppressedDnd,
  HeldForDesktop,
}

/// Devices that rang for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ring {
  pub conversation: ConversationId,
  pub targets: Vec<(UserId, DeviceId)>,
}

/// Where a notification is shown: a conversation's top-level messages or one of its threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ShownKey {
  user: UserId,
  conversation: ConversationId,
  maybe_thread: Option<ThreadId>,
}

#[derive(Debug, Clone, Copy)]
struct Shown {
  /// The newest message the device was woken up for.
  newest: MessageId,
  at: Instant,
}

#[derive(Default)]
struct Inner {
  shown: HashMap<ShownKey, HashMap<DeviceId, Shown>>,
  shown_entries: usize,
  rings: HashMap<CallId, (Ring, Instant)>,
  health: HashMap<DeviceId, (UserId, PushDeliveryCounts)>,
  last_test: HashMap<DeviceId, Instant>,
}

impl PushTracker {
  /// The device was woken up for `message` (a visible push the push service accepted).
  pub fn record_shown(
    &self,
    user: UserId,
    device: DeviceId,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    message: MessageId,
  ) {
    let now = Instant::now();
    let mut inner = self.lock();
    let key = ShownKey {
      user,
      conversation,
      maybe_thread,
    };
    let devices = inner.shown.entry(key).or_default();
    let is_new = !devices.contains_key(&device);
    let entry = devices.entry(device).or_insert(Shown {
      newest: message,
      at: now,
    });
    entry.newest = entry.newest.max(message);
    entry.at = now;
    if is_new {
      inner.shown_entries += 1;
      if inner.shown_entries > MAX_SHOWN_ENTRIES {
        inner.forget_oldest_shown();
      }
    }
  }

  /// The user read up to `up_to`: the devices whose notification for it is now fully read. They are
  /// forgotten; a device that was woken up for a newer message keeps its entry (and its notification).
  pub fn take_read(
    &self,
    user: UserId,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    up_to: MessageId,
  ) -> Vec<DeviceId> {
    let mut inner = self.lock();
    let key = ShownKey {
      user,
      conversation,
      maybe_thread,
    };
    let Some(devices) = inner.shown.get_mut(&key) else {
      return Vec::new();
    };
    let read: Vec<DeviceId> = devices
      .iter()
      .filter(|(_, shown)| shown.newest <= up_to)
      .map(|(device, _)| *device)
      .collect();
    for device in &read {
      devices.remove(device);
    }
    if devices.is_empty() {
      inner.shown.remove(&key);
    }
    inner.shown_entries = inner.shown_entries.saturating_sub(read.len());
    read
  }

  /// Remembers which devices ring for the call.
  pub fn record_ring(&self, call: CallId, ring: Ring) {
    let now = Instant::now();
    let mut inner = self.lock();
    if inner.rings.len() >= MAX_RINGS
      && let Some(oldest) = inner
        .rings
        .iter()
        .min_by_key(|(_, (_, at))| *at)
        .map(|(call, _)| *call)
    {
      inner.rings.remove(&oldest);
    }
    inner.rings.insert(call, (ring, now));
  }

  /// The devices that rang for the call; `None` once taken (the ring ends only once).
  pub fn take_ring(&self, call: CallId) -> Option<Ring> {
    self.lock().rings.remove(&call).map(|(ring, _)| ring)
  }

  pub fn record(&self, user: UserId, device: DeviceId, event: DeliveryEvent, now: UnixMillis) {
    let mut inner = self.lock();
    if inner.health.len() >= MAX_HEALTH_DEVICES && !inner.health.contains_key(&device) {
      // A bound against floods, not a policy: the counts restart for whoever comes next.
      inner.health.clear();
    }
    let (_, counts) = inner
      .health
      .entry(device)
      .or_insert_with(|| (user, PushDeliveryCounts::default()));
    match event {
      DeliveryEvent::Sent => {
        counts.sent += 1;
        counts.maybe_last_sent_at = Some(now);
      },
      DeliveryEvent::Failed => {
        counts.failed += 1;
        counts.maybe_last_failed_at = Some(now);
      },
      DeliveryEvent::Retried => counts.retried += 1,
      DeliveryEvent::Dismissed => counts.dismissed += 1,
      DeliveryEvent::SuppressedDnd => counts.suppressed_dnd += 1,
      DeliveryEvent::HeldForDesktop => counts.held_for_desktop += 1,
    }
  }

  /// The counts of one user's devices.
  pub fn counts_of(&self, user: UserId) -> HashMap<DeviceId, PushDeliveryCounts> {
    self
      .lock()
      .health
      .iter()
      .filter(|(_, (owner, _))| *owner == user)
      .map(|(device, (_, counts))| (*device, counts.clone()))
      .collect()
  }

  /// Whether a test push to the device may go out now (at most one per `interval`); if so, the
  /// time is remembered.
  pub fn try_start_test(&self, device: DeviceId, interval: Duration) -> bool {
    let now = Instant::now();
    let mut inner = self.lock();
    if inner
      .last_test
      .get(&device)
      .is_some_and(|at| now.duration_since(*at) < interval)
    {
      return false;
    }
    inner.last_test.retain(|_, at| now.duration_since(*at) < interval);
    inner.last_test.insert(device, now);
    true
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
    // A panic while holding the lock leaves plain counters behind; they stay usable.
    self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

impl Inner {
  /// Drops the oldest tenth of the notification entries.
  fn forget_oldest_shown(&mut self) {
    let mut ages: Vec<Instant> = self
      .shown
      .values()
      .flat_map(|devices| devices.values().map(|shown| shown.at))
      .collect();
    ages.sort_unstable();
    let Some(cutoff) = ages.get(ages.len() / 10).copied() else {
      return;
    };
    let mut removed = 0;
    self.shown.retain(|_, devices| {
      let before = devices.len();
      devices.retain(|_, shown| shown.at > cutoff);
      removed += before - devices.len();
      !devices.is_empty()
    });
    self.shown_entries = self.shown_entries.saturating_sub(removed);
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn reading_up_to_the_newest_pushed_message_takes_the_device() {
    let tracker = PushTracker::default();
    tracker.record_shown(user(1), device(1), conversation(1), None, message(10));
    tracker.record_shown(user(1), device(2), conversation(1), None, message(12));
    assert_eq!(
      tracker.take_read(user(1), conversation(1), None, message(11)),
      vec![device(1)]
    );
    // Device 2 shows a newer message: it keeps its notification until that is read too.
    assert!(
      tracker
        .take_read(user(1), conversation(1), None, message(11))
        .is_empty()
    );
    assert_eq!(
      tracker.take_read(user(1), conversation(1), None, message(12)),
      vec![device(2)]
    );
  }

  #[test]
  fn threads_and_the_conversation_are_separate() {
    let tracker = PushTracker::default();
    let thread = ThreadId::from_snowflake(Snowflake::new(5).unwrap());
    tracker.record_shown(user(1), device(1), conversation(1), Some(thread), message(10));
    assert!(
      tracker
        .take_read(user(1), conversation(1), None, message(99))
        .is_empty()
    );
    assert_eq!(
      tracker.take_read(user(1), conversation(1), Some(thread), message(10)),
      vec![device(1)]
    );
  }

  #[test]
  fn other_users_are_not_affected() {
    let tracker = PushTracker::default();
    tracker.record_shown(user(1), device(1), conversation(1), None, message(10));
    assert!(
      tracker
        .take_read(user(2), conversation(1), None, message(99))
        .is_empty()
    );
  }

  #[test]
  fn a_ring_ends_once() {
    let tracker = PushTracker::default();
    let call = CallId::from_snowflake(Snowflake::new(3).unwrap());
    let ring = Ring {
      conversation: conversation(1),
      targets: vec![(user(2), device(2))],
    };
    tracker.record_ring(call, ring.clone());
    assert_eq!(tracker.take_ring(call), Some(ring));
    assert_eq!(tracker.take_ring(call), None);
  }

  #[test]
  fn counts_are_kept_per_device_of_a_user() {
    let tracker = PushTracker::default();
    tracker.record(user(1), device(1), DeliveryEvent::Sent, UnixMillis(5));
    tracker.record(user(1), device(1), DeliveryEvent::Failed, UnixMillis(6));
    tracker.record(user(1), device(1), DeliveryEvent::SuppressedDnd, UnixMillis(7));
    tracker.record(user(2), device(2), DeliveryEvent::Sent, UnixMillis(8));
    let counts = tracker.counts_of(user(1));
    assert_eq!(counts.len(), 1);
    let one = &counts[&device(1)];
    assert_eq!((one.sent, one.failed, one.suppressed_dnd), (1, 1, 1));
    assert_eq!(one.maybe_last_sent_at, Some(UnixMillis(5)));
    assert_eq!(one.maybe_last_failed_at, Some(UnixMillis(6)));
  }

  #[tokio::test(start_paused = true)]
  async fn test_pushes_are_rate_limited_per_device() {
    let tracker = PushTracker::default();
    let interval = Duration::from_secs(10);
    assert!(tracker.try_start_test(device(1), interval));
    assert!(!tracker.try_start_test(device(1), interval));
    assert!(tracker.try_start_test(device(2), interval));
    tokio::time::advance(Duration::from_secs(10)).await;
    assert!(tracker.try_start_test(device(1), interval));
  }

  #[tokio::test(start_paused = true)]
  async fn the_oldest_entries_go_when_the_tracker_is_full() {
    let tracker = PushTracker::default();
    for n in 0..=MAX_SHOWN_ENTRIES as i64 {
      if n == 1 {
        tokio::time::advance(Duration::from_secs(1)).await;
      }
      tracker.record_shown(user(1), device(n + 1), conversation(1), None, message(10));
    }
    let entries = tracker.lock().shown_entries;
    assert!(entries < MAX_SHOWN_ENTRIES, "{entries}");
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn conversation(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn message(raw: i64) -> MessageId {
    MessageId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
