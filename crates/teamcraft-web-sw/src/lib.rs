//! The Rust half of the web client's service worker (docs/e2ee.md §7, §9.3).
//!
//! `trunk` builds this crate as a worker (wasm-bindgen target `no-modules`); `scripts/finish-sw.sh`
//! then inlines the glue into `sw.js` and names the wasm file there, so the worker only runs wasm
//! whose hash the signed release manifest lists. The exports are `handlePush`,
//! `handleNotificationClick` and `handleSubscriptionChange` of `teamcraft-core`.
#![cfg(target_arch = "wasm32")]

pub use teamcraft_core::web_push::{handle_notification_click, handle_push, handle_subscription_change};
