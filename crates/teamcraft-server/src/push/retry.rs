//! The durable retry queue (notifications.md, "Mobile push delivery"): wake-ups that failed for a
//! temporary reason wait in the database and are tried again with exponential backoff and jitter,
//! honouring the push service's `Retry-After`, until their TTL ends.
//!
//! A worker claims one due wake-up at a time with `SELECT ... FOR UPDATE SKIP LOCKED` and keeps the
//! row locked while it sends. A server that is killed mid-send drops its connection, Postgres
//! releases the lock, and the next worker (after a restart, or on another instance) takes the row
//! at once: nothing is lost, and no two workers send the same wake-up.

use std::time::Duration;

use rand_core::{OsRng, RngCore};
use teamcraft_db::{DbError, NewPushRetry, PushDeviceRow};
use teamcraft_proto::{DeviceId, PushKind, PushWakeUp, UserId};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::delivery;
use super::sender::{PushError, is_user_endpoint, now_millis};
use super::tracker::DeliveryEvent;
use crate::state::{AppState, RetryPolicy};

/// How often wake-ups whose TTL ended are removed.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Starts the retry workers (per pool, see [`RetryPolicy::workers`]) and the sweeper of expired
/// wake-ups. The handles let tests stop them like a killed process.
pub fn spawn_retry_workers(state: &AppState) -> Vec<JoinHandle<()>> {
  let workers = state.settings.get().push.retry.workers.max(1);
  let mut handles = Vec::with_capacity(workers * 2 + 1);
  for is_user_endpoint in [false, true] {
    for _ in 0..workers {
      handles.push(tokio::spawn(run_worker(state.clone(), is_user_endpoint)));
    }
  }
  handles.push(tokio::spawn(sweep_expired(state.clone())));
  handles
}

/// Puts the wake-up into the queue: `attempts` made so far, the next one after `delay`, all of
/// them within `ttl`. Returns whether it was queued; a full queue or a database error drops it.
pub(super) async fn enqueue(
  state: &AppState,
  user: UserId,
  device: &PushDeviceRow,
  wake_up: &PushWakeUp,
  attempts: u32,
  delay: Duration,
  ttl: Duration,
) -> bool {
  let id = match state.next_id() {
    Ok(id) => id,
    Err(error) => {
      warn!(%error, "no ID for a push retry");
      return false;
    },
  };
  let retry = NewPushRetry {
    id,
    user,
    device: device.device,
    is_user_endpoint: is_user_endpoint(&device.provider),
    wake_up: wake_up.clone(),
    attempts,
    delay,
    ttl,
  };
  let max_per_device = state.settings.get().push.retry.max_queued_per_device;
  match state.database.enqueue_push_retry(&retry, max_per_device).await {
    Ok(true) => {
      if delay.is_zero() {
        state.push.retry_signal().notify_waiters();
      }
      true
    },
    Ok(false) => {
      warn!(device_id = %device.device, "the device's push retry queue is full; wake-up dropped");
      false
    },
    Err(error) => {
      warn!(%error, device_id = %device.device, "queueing a push retry failed");
      false
    },
  }
}

/// The wait before the next attempt, after `attempts` failed ones: exponential from
/// `initial_delay` up to `max_delay`, with "equal jitter" (half fixed, half random, `jitter` in
/// `[0, 1)`) so the retries of many devices don't arrive in waves; at least what the push service
/// asked for. `None` when the retries are used up or the wake-up would expire first.
pub(super) fn next_delay(
  policy: &RetryPolicy,
  attempts: u32,
  maybe_retry_after: Option<Duration>,
  remaining_ttl: Duration,
  jitter: f64,
) -> Option<Duration> {
  if attempts > policy.max_retries {
    return None;
  }
  let exponent = attempts.saturating_sub(1).min(31);
  let backoff = policy
    .initial_delay
    .saturating_mul(1u32 << exponent)
    .min(policy.max_delay);
  let half = backoff / 2;
  let jittered = half + half.mul_f64(jitter.clamp(0.0, 1.0));
  let delay = maybe_retry_after.map_or(jittered, |asked| jittered.max(asked));
  (delay < remaining_ttl).then_some(delay)
}

/// A fresh jitter in `[0, 1)`.
pub(super) fn jitter() -> f64 {
  f64::from(OsRng.next_u32()) / (f64::from(u32::MAX) + 1.0)
}

async fn run_worker(state: AppState, is_user_endpoint: bool) {
  loop {
    match attempt_next(&state, is_user_endpoint).await {
      Ok(true) => {},
      Ok(false) => wait_for_work(&state).await,
      Err(error) => {
        warn!(%error, "the push retry queue failed");
        wait_for_work(&state).await;
      },
    }
  }
}

async fn wait_for_work(state: &AppState) {
  let poll_interval = state.settings.get().push.retry.poll_interval;
  tokio::select! {
    () = state.push.retry_signal().notified() => {},
    () = tokio::time::sleep(poll_interval) => {},
  }
}

/// Claims one due wake-up of the pool and makes the attempt. Returns whether there was one.
async fn attempt_next(state: &AppState, is_user_endpoint: bool) -> Result<bool, DbError> {
  let Some(claim) = state.database.claim_push_retry(is_user_endpoint).await? else {
    return Ok(false);
  };
  let row = claim.row().clone();
  let Some(device) = current_registration(state, row.user, row.device).await? else {
    debug!(device_id = %row.device, "the device has no push registration any more; retry dropped");
    claim.complete().await?;
    return Ok(true);
  };
  if !is_still_ringing(state, &row.wake_up) {
    debug!(device_id = %row.device, "the call stopped ringing meanwhile; retry dropped");
    claim.complete().await?;
    return Ok(true);
  }
  if is_read_meanwhile(state, row.user, &row.wake_up).await {
    debug!(device_id = %row.device, "the message was read meanwhile; retry dropped");
    claim.complete().await?;
    return Ok(true);
  }
  let result = delivery::send(state, row.user, &device, &row.wake_up, row.remaining_ttl).await;
  let attempts = row.attempts.saturating_add(1);
  match result {
    Ok(()) => {
      // Out of the queue first: a crash between the two at worst misses the counts.
      claim.complete().await?;
      debug!(device_id = %row.device, attempts, "push sent after a retry");
      delivery::on_sent(state, row.user, &device, &row.wake_up).await;
    },
    Err(PushError::Unavailable {
      reason,
      maybe_retry_after,
    }) => {
      let policy = state.settings.get().push.retry;
      match next_delay(&policy, attempts, maybe_retry_after, row.remaining_ttl, jitter()) {
        Some(delay) => {
          debug!(device_id = %row.device, %reason, attempts, delay_ms = delay.as_millis(), "push failed again");
          state
            .push_tracker
            .record(row.user, row.device, DeliveryEvent::Retried, now_millis());
          claim.reschedule(delay).await?;
        },
        None => {
          info!(device_id = %row.device, %reason, attempts, "push given up: retries used or TTL over");
          state
            .push_tracker
            .record(row.user, row.device, DeliveryEvent::Failed, now_millis());
          claim.complete().await?;
        },
      }
    },
    Err(error) => {
      claim.complete().await?;
      delivery::on_failed(state, row.user, &device, &error).await;
    },
  }
  Ok(true)
}

/// The device's registration as it is now (a new token or push key since the wake-up was queued
/// is used); `None` if the device is revoked or has none.
async fn current_registration(
  state: &AppState,
  user: UserId,
  device: DeviceId,
) -> Result<Option<PushDeviceRow>, DbError> {
  let devices = state.database.list_push_devices(user).await?;
  Ok(devices.into_iter().find(|row| row.device == device))
}

/// For a ring: whether its call still rings. Answered elsewhere, declined, cancelled or timed out,
/// a late retry would ring the phone for a call that is over (issue #142). Other kinds pass.
fn is_still_ringing(state: &AppState, wake_up: &PushWakeUp) -> bool {
  let (PushKind::Call, Some(call)) = (&wake_up.kind, wake_up.maybe_call_id) else {
    return true;
  };
  state
    .maybe_calls
    .as_ref()
    .is_some_and(|calls| calls.is_ringing(wake_up.conversation_id, call))
}

/// Whether the wake-up names a message the user has read since (on any device): showing it now
/// would only be noise. A failed check counts as unread; a duplicate beats a lost notification.
async fn is_read_meanwhile(state: &AppState, user: UserId, wake_up: &PushWakeUp) -> bool {
  let (true, Some(message)) = (
    matches!(wake_up.kind, PushKind::Message | PushKind::Mention),
    wake_up.maybe_message_id,
  ) else {
    return false;
  };
  match state
    .database
    .has_read_message(user, wake_up.conversation_id, message)
    .await
  {
    Ok(is_read) => is_read,
    Err(error) => {
      warn!(%error, "checking the read position failed; retrying anyway");
      false
    },
  }
}

async fn sweep_expired(state: AppState) {
  let mut interval = tokio::time::interval(SWEEP_INTERVAL);
  loop {
    interval.tick().await;
    match state.database.delete_expired_push_retries().await {
      Ok(0) => {},
      Ok(count) => info!(count, "push retries expired"),
      Err(error) => warn!(%error, "removing expired push retries failed"),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const HOUR: Duration = Duration::from_secs(3600);

  #[test]
  fn the_wait_doubles_up_to_the_cap() {
    let policy = policy();
    let waits: Vec<_> = (1..=6)
      .map(|attempts| next_delay(&policy, attempts, None, HOUR, 1.0).unwrap())
      .collect();
    let secs: Vec<_> = waits.iter().map(Duration::as_secs).collect();
    assert_eq!(secs, [2, 4, 8, 16, 30, 30]);
  }

  #[test]
  fn jitter_keeps_at_least_half_of_the_wait() {
    let policy = policy();
    assert_eq!(
      next_delay(&policy, 3, None, HOUR, 0.0),
      Some(Duration::from_secs(4))
    );
    assert_eq!(
      next_delay(&policy, 3, None, HOUR, 0.5),
      Some(Duration::from_secs(6))
    );
  }

  #[test]
  fn retry_after_is_honoured_even_beyond_the_cap() {
    let policy = policy();
    let asked = Duration::from_secs(120);
    assert_eq!(next_delay(&policy, 1, Some(asked), HOUR, 0.0), Some(asked));
    // A shorter hint never makes the wait shorter than the backoff.
    assert_eq!(
      next_delay(&policy, 4, Some(Duration::from_secs(1)), HOUR, 0.0),
      Some(Duration::from_secs(8))
    );
  }

  #[test]
  fn nothing_is_retried_past_the_ttl() {
    let policy = policy();
    assert_eq!(next_delay(&policy, 1, None, Duration::from_secs(1), 0.0), None);
    assert_eq!(
      next_delay(&policy, 1, Some(HOUR), Duration::from_secs(600), 0.0),
      None
    );
  }

  #[test]
  fn retries_end_after_the_maximum() {
    let policy = RetryPolicy {
      max_retries: 2,
      ..policy()
    };
    assert!(next_delay(&policy, 2, None, HOUR, 0.0).is_some());
    assert_eq!(next_delay(&policy, 3, None, HOUR, 0.0), None);
    let never = RetryPolicy {
      max_retries: 0,
      ..policy
    };
    assert_eq!(next_delay(&never, 1, None, HOUR, 0.0), None);
  }

  #[test]
  fn jitter_stays_in_range() {
    for _ in 0..1000 {
      let value = jitter();
      assert!((0.0..1.0).contains(&value), "{value}");
    }
  }

  #[test]
  fn huge_attempt_counts_do_not_overflow() {
    let policy = RetryPolicy {
      max_retries: u32::MAX,
      ..policy()
    };
    assert_eq!(
      next_delay(&policy, u32::MAX, None, HOUR, 1.0),
      Some(Duration::from_secs(30))
    );
  }

  fn policy() -> RetryPolicy {
    RetryPolicy {
      max_retries: 100,
      initial_delay: Duration::from_secs(2),
      max_delay: Duration::from_secs(30),
      workers: 1,
      max_queued_per_device: 10,
      poll_interval: Duration::from_secs(1),
    }
  }
}
