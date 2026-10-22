//! WebTransport SFU that forwards DAVE-encrypted RTP over QUIC datagrams (ARCHITECTURE.md §6).
//!
//! The SFU reads the TeamCraft datagram header and the RTP header (for the audio level) and never
//! decrypts or decodes media. In this spike it also acts as DAVE external sender, see SPIKE.md.

pub mod auth;
pub mod config;
pub mod control;
pub mod dave_coordinator;
pub mod dev_page;
pub mod forwarding;
pub mod health;
pub mod ids;
pub mod layers;
pub mod net;
pub mod prejoin;
pub mod reporting;
pub mod room;
pub mod server;
pub mod signaling;
pub mod speakers;
