//! The TeamCraft push gateway (docs/push-gateway.md): holds the APNs and FCM credentials of the
//! published store apps and forwards the sealed wake-ups of any TeamCraft server to them. It sees
//! device tokens, opaque server IDs and ciphertext; it cannot read or forge a wake-up.

pub mod app;
pub mod config;
pub mod limits;
pub mod maintenance;
pub mod metrics;
pub mod providers;
pub mod store;
