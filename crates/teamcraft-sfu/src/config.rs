//! SFU settings with their defaults.

use std::{
  net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
  time::Duration,
};

use ipnet::IpNet;
use teamcraft_media::fallback_queue::MediaQueueConfig;

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
  /// Devices of one account that may be in the same call at once. A further device is refused
  /// (`call_device_limit`); each one is a member of the call's DAVE group. The server enforces the
  /// same limit when it issues join tokens (`calls.max_devices_per_user_per_call`).
  pub max_devices_per_user_per_call: usize,
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
  /// The UDP socket's receive buffer. A big keyframe arrives as a burst of half a megabyte: a
  /// socket with the system's default buffer loses part of it while the SFU is busy, and with it
  /// the whole picture, for everyone watching.
  pub udp_receive_buffer_bytes: usize,
  /// The WebSocket fallback for networks that block UDP (ARCHITECTURE.md §6.6).
  pub fallback: FallbackConfig,
}

/// The fallback's listener and what one connection may cost.
#[derive(Debug, Clone)]
pub struct FallbackConfig {
  /// TCP address of the listener, plain HTTP behind the reverse proxy. IPv6 addresses are bound
  /// dual-stack. `None`: no fallback, the SFU speaks QUIC only (the default of this type, for tests;
  /// `main` opens it unless told not to).
  pub maybe_bind_addr: Option<SocketAddr>,
  /// Connections in a call at once. A further join is turned away without a verdict on the call: a
  /// client that is rejoining tries again. Mind the process's descriptor limit.
  pub max_connections: usize,
  /// Connections that haven't joined yet, at once; one more is closed before a byte is read. Anybody
  /// can open these, with no token: small, and apart from `max_connections`, so that a flood of them
  /// can't take a place from a call.
  pub max_unjoined_connections: usize,
  /// How long the WebSocket upgrade may take, before any token is seen. Enough for a slow link: a
  /// client that means it has sent the request as its first packet after TCP.
  pub handshake_timeout: Duration,
  /// What waits for one slow connection. Media beyond it, or older, is dropped.
  pub queue: MediaQueueConfig,
  /// The socket's send buffer. What is in it can't be dropped any more and has to go through, so a
  /// big one is seconds of latency on a narrow link; the kernel's auto-tuning grows it to megabytes.
  pub socket_send_buffer_bytes: usize,
  /// How often a connection is pinged: keeps proxies from closing a quiet call, and the answers
  /// prove the path alive. Under a second would be noise, over a few seconds too slow to notice a
  /// dead path by.
  pub ping_interval: Duration,
  /// A single write may take this long; after it the peer counts as gone. Bounds how long a slow
  /// peer can hold a connection, and all that belongs to it.
  pub write_timeout: Duration,
  /// The reverse proxies in front of this listener: only from these peers does the last address in
  /// `X-Forwarded-For` count for the per-address limits; from anybody else the header is ignored, so
  /// a client can't choose its address. The machine itself by default, as the default listener is
  /// only for it. A proxy not listed makes every client have its address and share one limit.
  pub trusted_proxies: Vec<IpNet>,
}

impl FallbackConfig {
  /// Whether `peer` (in its canonical form) is a proxy to take `X-Forwarded-For` from.
  pub fn is_trusted_proxy(&self, peer: IpAddr) -> bool {
    self.trusted_proxies.iter().any(|proxy| proxy.contains(&peer))
  }
}

/// `TEAMCRAFT_SFU_FALLBACK_TRUSTED_PROXIES`: addresses or networks, comma-separated; `None` for
/// something else.
pub fn parse_trusted_proxies(text: &str) -> Option<Vec<IpNet>> {
  text
    .split(',')
    .map(str::trim)
    .filter(|entry| !entry.is_empty())
    .map(|entry| {
      entry
        .parse::<IpNet>()
        .ok()
        .or_else(|| entry.parse::<IpAddr>().ok().map(IpNet::from))
    })
    .collect()
}

impl Default for FallbackConfig {
  fn default() -> Self {
    Self {
      maybe_bind_addr: None,
      max_connections: 1024,
      max_unjoined_connections: 128,
      handshake_timeout: Duration::from_secs(3),
      queue: MediaQueueConfig::default(),
      socket_send_buffer_bytes: 128 * 1024,
      ping_interval: Duration::from_secs(1),
      write_timeout: Duration::from_secs(5),
      trusted_proxies: vec![
        IpNet::from(IpAddr::from(Ipv4Addr::LOCALHOST)),
        IpNet::from(IpAddr::from(Ipv6Addr::LOCALHOST)),
      ],
    }
  }
}

impl Default for SfuConfig {
  fn default() -> Self {
    Self {
      bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 3478)),
      maybe_dev_http_addr: None,
      forwarding: ForwardingConfig::default(),
      join_timeout: Duration::from_secs(10),
      max_prejoin_sessions_per_ip: 16,
      max_devices_per_user_per_call: 3,
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
      udp_receive_buffer_bytes: 4 * 1024 * 1024,
      fallback: FallbackConfig::default(),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn trusted_proxies_are_addresses_or_networks() {
    let proxies = parse_trusted_proxies(" 10.0.0.1, 172.18.0.0/16,2001:db8::/64 ,").unwrap();
    let config = FallbackConfig {
      trusted_proxies: proxies,
      ..FallbackConfig::default()
    };
    for trusted in ["10.0.0.1", "172.18.3.4", "2001:db8::9"] {
      assert!(config.is_trusted_proxy(trusted.parse().unwrap()), "{trusted}");
    }
    for other in ["10.0.0.2", "172.19.0.1", "2001:db8:0:1::9", "127.0.0.1"] {
      assert!(!config.is_trusted_proxy(other.parse().unwrap()), "{other}");
    }
    assert!(parse_trusted_proxies("not an address").is_none());
    assert_eq!(parse_trusted_proxies(""), Some(Vec::new()));
  }

  #[test]
  fn only_the_machine_itself_is_trusted_by_default() {
    let config = FallbackConfig::default();
    assert!(config.is_trusted_proxy(Ipv4Addr::LOCALHOST.into()));
    assert!(config.is_trusted_proxy(Ipv6Addr::LOCALHOST.into()));
    assert!(!config.is_trusted_proxy("192.0.2.1".parse().unwrap()));
  }
}
