//! The push services TeamCraft sends sealed wake-ups to: APNs and FCM (e2ee.md §7).
//!
//! Shared by `teamcraft-server`, which uses these senders directly when an operator builds and
//! signs their own apps, and by `teamcraft-push-gateway`, which holds the credentials of the
//! published store apps for every other server (push-gateway.md). Both hand over the same thing:
//! a device token and a sealed wake-up. Neither the push service nor the gateway can read the
//! payload, and this crate never sees a user, device or key.

mod apns;
mod fcm;

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use teamcraft_proto::PushKind;

pub use apns::{APNS_PRODUCTION_URL, APNS_SANDBOX_URL, ApnsSender, ApnsSettings};
pub use fcm::{FCM_BASE_URL, FcmSender, FcmSettings, ServiceAccount};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
  Normal,
  /// Urgent messages: deliver immediately, even to a dozing device.
  High,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
  /// The registration is dead (unsubscribed, expired); the caller forgets it.
  #[error("the push registration is gone")]
  Gone,
  #[error("the push service rejected the request with status {0}")]
  Rejected(u16),
  #[error("the push service is unavailable: {0}")]
  Unavailable(String),
  #[error("the registration is invalid: {0}")]
  Invalid(String),
}

/// Sends one wake-up to one device token of one push service.
pub trait TokenSender: Send + Sync {
  fn send<'a>(&'a self, token: &'a str, message: &'a PushMessage)
  -> BoxFuture<'a, Result<(), ProviderError>>;
}

/// Where and what kind of JSON error, without serde's quote of the input (which can be a user's
/// endpoint or, for key files, a private key).
fn json_error(error: &serde_json::Error) -> String {
  format!(
    "{:?} error at line {}, column {}",
    error.classify(),
    error.line(),
    error.column()
  )
}
