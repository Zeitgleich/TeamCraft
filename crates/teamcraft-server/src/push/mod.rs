//! Push notifications (e2ee.md §7): encrypted wake-ups through Web Push, APNs, FCM and UnifiedPush.
//!
//! A wake-up names the conversation and message but carries no content; the app fetches and decrypts
//! the message itself. The payload is sealed with the device's push key, so push services only see
//! ciphertext.

pub mod crypto;
pub mod routing;
mod sender;
mod web_push;

pub use sender::{
  ApnsSender, FcmSender, PushDispatcher, PushError, PushMessage, PushSender, PushTarget, UnifiedPushSender,
  Urgency,
};
pub use web_push::{WebPushSender, WebPushSubscription};
