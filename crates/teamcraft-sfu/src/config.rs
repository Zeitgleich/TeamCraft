//! SFU settings with their defaults.

use std::{
  net::{Ipv6Addr, SocketAddr},
  time::Duration,
};

#[derive(Debug, Clone)]
pub struct SfuConfig {
  /// UDP address of the WebTransport endpoint. IPv6 addresses are bound dual-stack
  /// (`IPV6_V6ONLY` off), so the default `[::]` also accepts IPv4 clients as IPv4-mapped addresses
  /// (ARCHITECTURE.md §1.2).
  pub bind_addr: SocketAddr,
  /// Serves the browser test page over plain HTTP when set. Development only: it also enables the
  /// [`crate::auth::DevTokenVerifier`] in `main`.
  pub maybe_dev_http_addr: Option<SocketAddr>,
  /// Audio of at most this many loudest speakers is forwarded (ARCHITECTURE.md §6.3: 3–5).
  pub max_forwarded_speakers: usize,
  /// A participant whose audio stopped this long ago no longer counts as a speaker.
  pub speaker_timeout: Duration,
  /// Time a new session has to send its `join` message.
  pub join_timeout: Duration,
  /// Upper bound for one signaling message.
  pub max_signaling_message_bytes: usize,
}

impl Default for SfuConfig {
  fn default() -> Self {
    Self {
      bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 4433)),
      maybe_dev_http_addr: None,
      max_forwarded_speakers: 3,
      speaker_timeout: Duration::from_millis(1000),
      join_timeout: Duration::from_secs(10),
      max_signaling_message_bytes: 256 * 1024,
    }
  }
}
