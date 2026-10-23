//! Sending one wake-up to one device (notifications.md, "Mobile push delivery"): priority and TTL
//! by kind, retries after temporary failures, the tracker's counts, and forgetting registrations
//! the push service says are dead.

use teamcraft_db::PushDeviceRow;
use teamcraft_proto::{PushKind, PushProvider, PushWakeUp, UserId};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::sender::{PushError, PushTarget, Urgency, now_millis};
use super::tracker::DeliveryEvent;
use crate::state::AppState;

/// Sends in the background; the outcome only goes to the logs and the counts.
pub(crate) fn spawn(state: AppState, user: UserId, device: PushDeviceRow, wake_up: PushWakeUp) {
  tokio::spawn(async move {
    // Logged and counted inside.
    let _ = deliver(&state, user, &device, &wake_up).await;
  });
}

/// Sends now, trying again after temporary failures while the wake-up's TTL lasts.
pub(crate) async fn deliver(
  state: &AppState,
  user: UserId,
  device: &PushDeviceRow,
  wake_up: &PushWakeUp,
) -> Result<(), PushError> {
  deliver_with_retries(
    state,
    user,
    device,
    wake_up,
    state.settings.push.retry.max_retries,
  )
  .await
}

/// Sends once, for callers that answer with the outcome (test pushes).
pub(crate) async fn deliver_once(
  state: &AppState,
  user: UserId,
  device: &PushDeviceRow,
  wake_up: &PushWakeUp,
) -> Result<(), PushError> {
  deliver_with_retries(state, user, device, wake_up, 0).await
}

async fn deliver_with_retries(
  state: &AppState,
  user: UserId,
  device: &PushDeviceRow,
  wake_up: &PushWakeUp,
  max_retries: u32,
) -> Result<(), PushError> {
  let target = PushTarget {
    user,
    device: device.device,
    provider: device.provider.clone(),
    token: device.token.clone(),
    push_key: device.push_key.clone(),
  };
  let ttl = state.settings.push.ttls.for_kind(&wake_up.kind);
  let started = Instant::now();
  let mut delay = state.settings.push.retry.initial_delay;
  let mut attempt = 0;
  loop {
    let result = state
      .push
      .deliver(&target, wake_up, urgency(&wake_up.kind), ttl)
      .await;
    match result {
      Ok(()) => {
        debug!(device_id = %target.device, kind = %wake_up.kind, "push sent");
        record_sent(state, user, device, wake_up);
        return Ok(());
      },
      Err(PushError::Unavailable(reason)) if attempt < max_retries && started.elapsed() + delay < ttl => {
        debug!(device_id = %target.device, %reason, attempt, "push failed for now, trying again");
        state
          .push_tracker
          .record(user, target.device, DeliveryEvent::Retried, now_millis());
        tokio::time::sleep(delay).await;
        delay = delay.saturating_mul(2);
        attempt += 1;
      },
      Err(error) => {
        state
          .push_tracker
          .record(user, target.device, DeliveryEvent::Failed, now_millis());
        log_failure(state, user, &target, &error).await;
        return Err(error);
      },
    }
  }
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

async fn log_failure(state: &AppState, user: UserId, target: &PushTarget, error: &PushError) {
  match error {
    PushError::Gone => {
      info!(device_id = %target.device, "push registration is gone, forgetting it");
      if let Err(error) = state.database.clear_push_registration(user, target.device).await {
        warn!(%error, "clearing the push registration failed");
      }
    },
    PushError::NotConfigured => debug!(provider = %target.provider, "push provider unavailable"),
    error => warn!(%error, device_id = %target.device, "push failed"),
  }
}
