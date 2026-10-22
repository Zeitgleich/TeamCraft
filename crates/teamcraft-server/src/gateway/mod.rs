//! WebSocket gateway: sessions, event fan-out, resume and presence.

pub mod connection;
mod hub;

pub use hub::{ActiveDevices, Attached, DeviceAggregate, Hub, HubOptions, PresenceSnapshot, ResumeError};
