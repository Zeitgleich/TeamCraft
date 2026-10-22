use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use teamcraft_proto::{DeviceId, PushProvider, PushWakeUp, UserId};
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use super::crypto::{PushCryptoError, seal_wakeup};
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
  #[error("sending to {0} is not implemented yet")]
  NotImplemented(&'static str),
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
}

impl PushDispatcher {
  pub fn new(max_concurrent_sends: usize) -> Self {
    Self {
      senders: HashMap::new(),
      permits: Arc::new(Semaphore::new(max_concurrent_sends.max(1))),
    }
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
    let sender = known_name(&target.provider)
      .and_then(|name| self.senders.get(name))
      .ok_or(PushError::NotConfigured)?;
    let message = PushMessage {
      sealed: seal_wakeup(&target.push_key, wake_up)?,
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
    sender.send(target, &message).await
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

/// APNs (HTTP/2 with a provider token or certificate): not implemented yet.
pub struct ApnsSender;

/// FCM (HTTP v1 with a service account): not implemented yet.
pub struct FcmSender;

/// UnifiedPush (a POST to the distributor's endpoint): not implemented yet.
pub struct UnifiedPushSender;

macro_rules! stub_sender {
  ($sender:ty, $name:literal) => {
    impl PushSender for $sender {
      fn send<'a>(
        &'a self,
        _target: &'a PushTarget,
        _message: &'a PushMessage,
      ) -> BoxFuture<'a, Result<(), PushError>> {
        Box::pin(async {
          warn!(provider = $name, "push provider is a stub, nothing was sent");
          Err(PushError::NotImplemented($name))
        })
      }
    }
  };
}

stub_sender!(ApnsSender, "apns");
stub_sender!(FcmSender, "fcm");
stub_sender!(UnifiedPushSender, "unifiedpush");

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
  async fn the_stubs_report_that_they_are_not_implemented() {
    let dispatcher = PushDispatcher::new(4)
      .with_sender(&PushProvider::Apns, Arc::new(ApnsSender))
      .with_sender(&PushProvider::Fcm, Arc::new(FcmSender))
      .with_sender(&PushProvider::UnifiedPush, Arc::new(UnifiedPushSender));
    for provider in [PushProvider::Apns, PushProvider::Fcm, PushProvider::UnifiedPush] {
      let result = dispatcher
        .deliver(
          &target(provider),
          &wake_up(),
          Urgency::High,
          Duration::from_secs(60),
        )
        .await;
      assert!(matches!(result, Err(PushError::NotImplemented(_))));
    }
  }

  #[tokio::test]
  async fn a_bad_push_key_fails_before_anything_is_sent() {
    let dispatcher = PushDispatcher::new(4).with_sender(&PushProvider::Apns, Arc::new(ApnsSender));
    let mut bad = target(PushProvider::Apns);
    bad.push_key = vec![1, 2, 3];
    let result = dispatcher
      .deliver(&bad, &wake_up(), Urgency::Normal, Duration::from_secs(60))
      .await;
    assert!(matches!(result, Err(PushError::Crypto(_))));
  }
}
