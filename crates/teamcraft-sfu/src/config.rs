//! SFU settings with their defaults.

use std::{
  net::{Ipv6Addr, SocketAddr},
  time::Duration,
};

use crate::forwarding::ForwardingConfig;

#[derive(Debug, Clone)]
pub struct SfuConfig {
  /// UDP address of the WebTransport endpoint. IPv6 addresses are bound dual-stack
  /// (`IPV6_V6ONLY` off), so the default `[::]` also accepts IPv4 clients as IPv4-mapped addresses
  /// (ARCHITECTURE.md §1.2).
  pub bind_addr: SocketAddr,
  /// Serves the browser test page over plain HTTP when set. Development only: it also enables the
  /// [`crate::auth::DevTokenVerifier`] in `main`.
  pub maybe_dev_http_addr: Option<SocketAddr>,
  /// Speaker selection, layer allocation and bandwidth estimation.
  pub forwarding: ForwardingConfig,
  /// Time a new session has to send its `join` message.
  pub join_timeout: Duration,
  /// Unjoined sessions one source address may hold at the same time.
  pub max_prejoin_sessions_per_ip: usize,
  /// Signaling events queued for one slow participant before it is disconnected.
  pub signaling_queue_len: usize,
  /// Upper bound for one signaling message.
  pub max_signaling_message_bytes: usize,
  /// Tolerated clock difference to the server when checking join token times.
  pub join_token_leeway: Duration,
  /// Join tokens valid for longer than this are rejected.
  pub max_join_token_lifetime: Duration,
  /// QUIC keep-alive. Mobile carriers' NATs drop idle UDP mappings after about 30 s (some after
  /// 20 s); this keeps them open in quiet calls and makes a dead path show within the idle timeout.
  pub keep_alive_interval: Duration,
  /// A connection that receives nothing for this long is lost (the shorter of this and the
  /// client's setting applies). Its participant then keeps the slot for `reconnect_grace`.
  pub idle_timeout: Duration,
  /// How long a participant whose connection was lost keeps its slot to rejoin; the others see
  /// it as reconnecting meanwhile. Clients learn it with `joined`.
  pub reconnect_grace: Duration,
}

impl Default for SfuConfig {
  fn default() -> Self {
    Self {
      bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 4433)),
      maybe_dev_http_addr: None,
      forwarding: ForwardingConfig::default(),
      join_timeout: Duration::from_secs(10),
      max_prejoin_sessions_per_ip: 16,
      signaling_queue_len: 256,
      max_signaling_message_bytes: 256 * 1024,
      join_token_leeway: Duration::from_secs(5),
      max_join_token_lifetime: Duration::from_secs(300),
      keep_alive_interval: Duration::from_secs(2),
      idle_timeout: Duration::from_secs(10),
      reconnect_grace: Duration::from_secs(30),
    }
  }
}
