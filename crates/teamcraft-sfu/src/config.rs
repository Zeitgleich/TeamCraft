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
  /// Mute changes one participant may report at once; more are ignored until the rate refills.
  pub mute_report_burst: u32,
  /// Mute changes one participant may report per second, beyond the burst.
  pub mute_reports_per_second: u32,
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
  /// On shutdown, how long the going-away closes get to reach the clients before the process
  /// exits (clients that miss it notice the dead path within seconds anyway).
  pub shutdown_drain: Duration,
  /// How often the SFU reports its whole roster to the server (also once at start and after a call
  /// moved between devices), so a leave the server never heard of can't keep a device in a call
  /// for good. One small request per interval; the server drops what the SFU no longer has.
  pub roster_interval: Duration,
}

impl Default for SfuConfig {
  fn default() -> Self {
    Self {
      bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 3478)),
      maybe_dev_http_addr: None,
      forwarding: ForwardingConfig::default(),
      join_timeout: Duration::from_secs(10),
      max_prejoin_sessions_per_ip: 16,
      signaling_queue_len: 256,
      mute_report_burst: 10,
      mute_reports_per_second: 2,
      max_signaling_message_bytes: 256 * 1024,
      join_token_leeway: Duration::from_secs(5),
      max_join_token_lifetime: Duration::from_secs(300),
      keep_alive_interval: Duration::from_secs(2),
      idle_timeout: Duration::from_secs(10),
      reconnect_grace: Duration::from_secs(30),
      shutdown_drain: Duration::from_secs(2),
      roster_interval: Duration::from_secs(30),
    }
  }
}
