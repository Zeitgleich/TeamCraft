//! Push notifications (e2ee.md §7): encrypted wake-ups through Web Push, APNs, FCM and UnifiedPush.
//!
//! A wake-up names the conversation and message but carries no content; the app fetches and decrypts
//! the message itself. The payload is sealed with the device's push key, so push services only see
//! ciphertext.

mod apns;
pub mod crypto;
mod fcm;
pub mod routing;
mod sender;
mod status;
mod unified_push;
mod web_push;

pub use apns::{APNS_PRODUCTION_URL, APNS_SANDBOX_URL, ApnsSender, ApnsSettings};
pub use fcm::{FCM_BASE_URL, FcmSender, FcmSettings, ServiceAccount};
pub use sender::{PushDispatcher, PushError, PushMessage, PushSender, PushTarget, Urgency};
pub use status::ProviderHealth;
pub use unified_push::UnifiedPushSender;
pub use web_push::{WebPushSender, WebPushSubscription};
