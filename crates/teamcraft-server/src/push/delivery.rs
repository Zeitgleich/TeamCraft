//! Sending one wake-up to one device (notifications.md, "Mobile push delivery"): priority and TTL
//! by kind, temporary failures into the durable retry queue, the tracker's counts, and forgetting
//! registrations the push service says are dead.

use std::time::Duration;

use teamcraft_db::PushDeviceRow;
use teamcraft_proto::{PushKind, PushProvider, PushWakeUp, UserId};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::retry;
use super::sender::{PushError, PushTarget, Urgency, now_millis};
use super::tracker::DeliveryEvent;
use crate::state::AppState;

/// Sends in the background; the outcome only goes to the logs and the counts.
pub(crate) fn spawn(state: AppState, user: UserId, device: PushDeviceRow, wake_up: PushWakeUp) {
  tokio::spawn(async move { deliver(&state, user, &device, &wake_up).await });
}

/// Sends now; a temporary failure goes into the retry queue for the rest of the wake-up's TTL.
/// While the provider is failing anyway, the wake-up is queued before the attempt, so a crash
/// during an outage loses nothing; a worker makes the attempt at once.
async fn deliver(state: &AppState, user: UserId, device: &PushDeviceRow, wake_up: &PushWakeUp) {
  let settings = state.settings.get();
  let ttl = settings.push.ttls.for_kind(&wake_up.kind);
  let policy = settings.push.retry;
  let can_retry = policy.max_retries > 0;
  if can_retry
    && state.push.health(&device.provider).is_failing()
    && retry::enqueue(state, user, device, wake_up, 0, Duration::ZERO, ttl).await
  {
    debug!(device_id = %device.device, "push provider failing; wake-up queued first");
    return;
  }
  let started = Instant::now();
  match send(state, user, device, wake_up, ttl).await {
    Ok(()) => on_sent(state, user, device, wake_up).await,
    Err(PushError::Unavailable {
      reason,
      maybe_retry_after,
    }) => {
      let remaining = ttl.saturating_sub(started.elapsed());
      let maybe_delay = retry::next_delay(&policy, 1, maybe_retry_after, remaining, retry::jitter());
      let is_queued = match maybe_delay {
        Some(delay) => retry::enqueue(state, user, device, wake_up, 1, delay, remaining).await,
        None => false,
      };
      let event = if is_queued {
        debug!(device_id = %device.device, %reason, "push failed for now, queued for a retry");
        DeliveryEvent::Retried
      } else {
        warn!(device_id = %device.device, %reason, "push failed and is not retried");
        DeliveryEvent::Failed
      };
      state
        .push_tracker
        .record(user, device.device, event, now_millis());
    },
    Err(error) => on_failed(state, user, device, &error).await,
  }
}

/// Sends once, for callers that answer with the outcome (test pushes): no retries.
pub(crate) async fn deliver_once(
  state: &AppState,
  user: UserId,
  device: &PushDeviceRow,
  wake_up: &PushWakeUp,
) -> Result<(), PushError> {
  let ttl = state.settings.get().push.ttls.for_kind(&wake_up.kind);
  let result = send(state, user, device, wake_up, ttl).await;
  match &result {
    Ok(()) => on_sent(state, user, device, wake_up).await,
    Err(error) => on_failed(state, user, device, error).await,
  }
  result
}

/// One attempt to the device's registration; the push service may hold it for `ttl`.
pub(super) async fn send(
  state: &AppState,
  user: UserId,
  device: &PushDeviceRow,
  wake_up: &PushWakeUp,
  ttl: Duration,
) -> Result<(), PushError> {
  let target = PushTarget {
    user,
    device: device.device,
    provider: device.provider.clone(),
    token: device.token.clone(),
    push_key: device.push_key.clone(),
  };
  state
    .push
    .deliver(&target, wake_up, urgency(&wake_up.kind), ttl)
    .await
}

/// After a successful send: the counts, and a withdrawal makes queued wake-ups it covers pointless.
pub(super) async fn on_sent(state: &AppState, user: UserId, device: &PushDeviceRow, wake_up: &PushWakeUp) {
  debug!(device_id = %device.device, kind = %wake_up.kind, "push sent");
  record_sent(state, user, device, wake_up);
  if wake_up.kind == PushKind::Dismiss
    && let Err(error) = state
      .database
      .delete_superseded_push_retries(device.device, wake_up)
      .await
  {
    warn!(%error, "dropping superseded push retries failed");
  }
}

/// After a failure that no retry fixes.
pub(super) async fn on_failed(state: &AppState, user: UserId, device: &PushDeviceRow, error: &PushError) {
  state
    .push_tracker
    .record(user, device.device, DeliveryEvent::Failed, now_millis());
  log_failure(state, user, device, error).await;
}

/// Visible wake-ups may wake a dozing phone (FCM `HIGH`, APNs priority 10); a withdrawal never needs
/// to: it is silent and can wait for the next window.
fn urgency(kind: &PushKind) -> Urgency {
  if kind.is_visible() {
    Urgency::High
  } else {
    Urgency::Normal
  }
}

/// Whether the device's provider can deliver a silent withdrawal. Browsers must show a notification
/// for every push, so Web Push gets none (the service worker replaces notifications by tag instead).
pub(crate) fn can_dismiss(provider: &PushProvider) -> bool {
  matches!(
    provider,
    PushProvider::Apns | PushProvider::Fcm | PushProvider::UnifiedPush
  )
}

fn record_sent(state: &AppState, user: UserId, device: &PushDeviceRow, wake_up: &PushWakeUp) {
  let event = if wake_up.kind == PushKind::Dismiss {
    DeliveryEvent::Dismissed
  } else {
    DeliveryEvent::Sent
  };
  state
    .push_tracker
    .record(user, device.device, event, now_millis());
  let is_message = matches!(wake_up.kind, PushKind::Message | PushKind::Mention);
  if let (true, true, Some(message)) = (
    is_message,
    can_dismiss(&device.provider),
    wake_up.maybe_message_id,
  ) {
    state.push_tracker.record_shown(
      user,
      device.device,
      wake_up.conversation_id,
      wake_up.maybe_thread_id,
      message,
    );
  }
}

async fn log_failure(state: &AppState, user: UserId, device: &PushDeviceRow, error: &PushError) {
  match error {
    PushError::Gone => {
      info!(device_id = %device.device, "push registration is gone, forgetting it");
      if let Err(error) = state.database.clear_push_registration(user, device.device).await {
        warn!(%error, "clearing the push registration failed");
      }
    },
    PushError::NotConfigured => debug!(provider = %device.provider, "push provider unavailable"),
    error => warn!(%error, device_id = %device.device, "push failed"),
  }
}
