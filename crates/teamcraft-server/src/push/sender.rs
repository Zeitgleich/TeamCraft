use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use teamcraft_proto::{DeviceId, PushKind, PushProvider, PushWakeUp, UnixMillis, UserId};
use tokio::sync::Semaphore;
use tracing::debug;

use super::crypto::{PushCryptoError, seal_wakeup};
use super::status::{ProviderHealth, ProviderHealthTracker};
use crate::auth::BoxFuture;

/// Where a push goes: one registered device.
#[derive(Clone)]
pub struct PushTarget {
  pub user: UserId,
  pub device: DeviceId,
  pub provider: PushProvider,
  /// Provider-specific: APNs/FCM token, UnifiedPush endpoint, Web Push subscription JSON.
  pub token: String,
  /// The 256-bit key shared with the device; seals the payload.
  pub push_key: Vec<u8>,
}

// The token and the push key are secrets: never printed, wiped from memory when the target goes.
impl std::fmt::Debug for PushTarget {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PushTarget")
      .field("user", &self.user)
      .field("device", &self.device)
      .field("provider", &self.provider)
      .field("token", &"[redacted]")
      .field("push_key", &"[redacted]")
      .finish()
  }
}

impl Drop for PushTarget {
  fn drop(&mut self) {
    use zeroize::Zeroize as _;
    self.token.zeroize();
    self.push_key.zeroize();
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
  Normal,
  /// Urgent messages: deliver immediately, even to a dozing device.
  High,
}

/// An encrypted wake-up ready to be sent.
#[derive(Debug, Clone)]
pub struct PushMessage {
  /// `seal_wakeup` output: opaque to push services.
  pub sealed: Vec<u8>,
  /// What the wake-up is for. Push services only need it to pick a channel (APNs: VoIP for calls).
  pub kind: PushKind,
  pub urgency: Urgency,
  /// How long the push service may hold it if the device is offline.
  pub ttl: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum PushError {
  /// The registration is dead (unsubscribed, expired); the server forgets it.
  #[error("the push registration is gone")]
  Gone,
  #[error("the push service rejected the request with status {0}")]
  Rejected(u16),
  #[error("the push service is unavailable: {0}")]
  Unavailable(String),
  #[error("this provider is not configured")]
  NotConfigured,
  #[error("the registration is invalid: {0}")]
  Invalid(String),
  #[error(transparent)]
  Crypto(#[from] PushCryptoError),
}

/// Sends one push through one provider. Implementations get the sealed wake-up and wrap it the way
/// their service wants it.
pub trait PushSender: Send + Sync {
  fn send<'a>(
    &'a self,
    target: &'a PushTarget,
    message: &'a PushMessage,
  ) -> BoxFuture<'a, Result<(), PushError>>;
}

/// Routes pushes to the sender of the target's provider, with a cap on parallel requests.
pub struct PushDispatcher {
  senders: HashMap<&'static str, Arc<dyn PushSender>>,
  permits: Arc<Semaphore>,
  health: ProviderHealthTracker,
  /// The VAPID public key browsers subscribe with (`applicationServerKey`), if Web Push is on.
  maybe_web_public_key: Option<String>,
}

impl PushDispatcher {
  pub fn new(max_concurrent_sends: usize) -> Self {
    Self {
      senders: HashMap::new(),
      permits: Arc::new(Semaphore::new(max_concurrent_sends.max(1))),
      health: ProviderHealthTracker::default(),
      maybe_web_public_key: None,
    }
  }

  /// Publishes the VAPID public key to browsers (`GET /api/v1/push/web-key`).
  pub fn with_web_public_key(mut self, public_key: String) -> Self {
    self.maybe_web_public_key = Some(public_key);
    self
  }

  /// Whether the provider has a sender.
  pub fn is_configured(&self, provider: &PushProvider) -> bool {
    known_name(provider).is_some_and(|name| self.senders.contains_key(name))
  }

  /// What this process has seen from the provider's push service since it started.
  pub fn health(&self, provider: &PushProvider) -> ProviderHealth {
    known_name(provider)
      .map(|name| self.health.snapshot(name))
      .unwrap_or_default()
  }

  pub fn web_public_key(&self) -> Option<&str> {
    self.maybe_web_public_key.as_deref()
  }

  /// Registers the sender for a provider (replacing a previous one).
  pub fn with_sender(mut self, provider: &PushProvider, sender: Arc<dyn PushSender>) -> Self {
    if let Some(name) = known_name(provider) {
      self.senders.insert(name, sender);
    }
    self
  }

  /// Seals the wake-up with the device's push key and sends it.
  pub async fn deliver(
    &self,
    target: &PushTarget,
    wake_up: &PushWakeUp,
    urgency: Urgency,
    ttl: Duration,
  ) -> Result<(), PushError> {
    let name = known_name(&target.provider).ok_or(PushError::NotConfigured)?;
    let sender = self.senders.get(name).ok_or(PushError::NotConfigured)?;
    let message = PushMessage {
      sealed: seal_wakeup(&target.push_key, wake_up)?,
      kind: wake_up.kind.clone(),
      urgency,
      ttl,
    };
    // The semaphore is never closed, so acquiring only fails if the dispatcher is gone.
    let _permit = self
      .permits
      .acquire()
      .await
      .map_err(|_| PushError::Unavailable("shutting down".into()))?;
    debug!(provider = %target.provider, device_id = %target.device, "sending push");
    let result = sender.send(target, &message).await;
    self.health.record(name, &result, now_millis());
    result
  }
}

fn known_name(provider: &PushProvider) -> Option<&'static str> {
  match provider {
    PushProvider::Apns => Some("apns"),
    PushProvider::Fcm => Some("fcm"),
    PushProvider::UnifiedPush => Some("unifiedpush"),
    PushProvider::WebPush => Some("webpush"),
    PushProvider::Unknown(_) => None,
  }
}

fn now_millis() -> UnixMillis {
  let millis = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |elapsed| elapsed.as_millis());
  UnixMillis(i64::try_from(millis).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{ConversationId, Id, PushKind, Snowflake};

  use super::*;

  #[test]
  fn a_push_target_prints_no_secrets() {
    let mut secret = target(PushProvider::Fcm);
    secret.token = "device-token-SECRET".into();
    secret.push_key = vec![0xAB; 32];
    let text = format!("{secret:?}");
    assert!(
      !text.contains("SECRET") && !text.contains("171") && !text.contains("ab"),
      "{text}"
    );
    assert!(text.contains("[redacted]"));
  }

  fn target(provider: PushProvider) -> PushTarget {
    PushTarget {
      user: UserId::from_snowflake(Snowflake::new(1).unwrap()),
      device: DeviceId::from_snowflake(Snowflake::new(2).unwrap()),
      provider,
      token: "t".into(),
      push_key: vec![5u8; 32],
    }
  }

  fn wake_up() -> PushWakeUp {
    PushWakeUp {
      kind: PushKind::Message,
      conversation_id: ConversationId::from_snowflake(Snowflake::new(3).unwrap()),
      maybe_message_id: None,
      maybe_sender_id: None,
      is_urgent: false,
    }
  }

  #[tokio::test]
  async fn providers_without_a_sender_are_not_configured() {
    let dispatcher = PushDispatcher::new(4);
    let result = dispatcher
      .deliver(
        &target(PushProvider::Fcm),
        &wake_up(),
        Urgency::Normal,
        Duration::from_secs(60),
      )
      .await;
    assert!(matches!(result, Err(PushError::NotConfigured)));
    let unknown = dispatcher
      .deliver(
        &target(PushProvider::Unknown("x".into())),
        &wake_up(),
        Urgency::Normal,
        Duration::from_secs(60),
      )
      .await;
    assert!(matches!(unknown, Err(PushError::NotConfigured)));
  }

  #[tokio::test]
  async fn deliveries_are_counted_per_provider() {
    let dispatcher = PushDispatcher::new(4)
      .with_sender(&PushProvider::Fcm, Arc::new(Fixed(Ok(()))))
      .with_sender(&PushProvider::Apns, Arc::new(Fixed(Err(503))))
      .with_sender(&PushProvider::UnifiedPush, Arc::new(Fixed(Err(410))));
    for provider in [
      PushProvider::Fcm,
      PushProvider::Fcm,
      PushProvider::Apns,
      PushProvider::UnifiedPush,
    ] {
      let _ = dispatcher
        .deliver(
          &target(provider),
          &wake_up(),
          Urgency::High,
          Duration::from_secs(60),
        )
        .await;
    }
    let fcm = dispatcher.health(&PushProvider::Fcm);
    assert_eq!((fcm.success_count, fcm.failure_count), (2, 0));
    assert!(fcm.maybe_last_success_at.is_some() && !fcm.is_failing());
    let apns = dispatcher.health(&PushProvider::Apns);
    assert_eq!((apns.success_count, apns.failure_count), (0, 1));
    assert!(apns.is_failing() && apns.maybe_last_error.is_some());
    // A dead registration says nothing against the push service.
    let unified = dispatcher.health(&PushProvider::UnifiedPush);
    assert_eq!((unified.success_count, unified.failure_count), (1, 0));
  }

  /// Succeeds, or fails with the status (410 is a dead registration, the rest unavailable).
  struct Fixed(Result<(), u16>);

  impl PushSender for Fixed {
    fn send<'a>(&'a self, _: &'a PushTarget, _: &'a PushMessage) -> BoxFuture<'a, Result<(), PushError>> {
      Box::pin(async move {
        match self.0 {
          Ok(()) => Ok(()),
          Err(410) => Err(PushError::Gone),
          Err(status) => Err(PushError::Unavailable(format!("status {status}"))),
        }
      })
    }
  }

  #[tokio::test]
  async fn a_bad_push_key_fails_before_anything_is_sent() {
    let dispatcher = PushDispatcher::new(4).with_sender(&PushProvider::Apns, Arc::new(Fixed(Ok(()))));
    let mut bad = target(PushProvider::Apns);
    bad.push_key = vec![1, 2, 3];
    let result = dispatcher
      .deliver(&bad, &wake_up(), Urgency::Normal, Duration::from_secs(60))
      .await;
    assert!(matches!(result, Err(PushError::Crypto(_))));
  }
}
