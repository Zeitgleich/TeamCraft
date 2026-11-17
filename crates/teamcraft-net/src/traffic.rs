//! Marking a connection's packets for the queues on the way (DSCP, RFC 2474), #1989.
//!
//! A home router's Wi-Fi (WMM) and many office switches serve packets marked Expedited Forwarding
//! (EF, RFC 3246) first, in the voice queue: a voice that shares a congested uplink with video and
//! downloads gets through. Only connections that carry nothing but voice are marked EF: video or
//! bulk data marked so would crowd out the voice it is meant to protect (RFC 8837). Most of the
//! Internet ignores or clears the mark; the first hop, often the one that queues, honours it.

use std::io;
use std::net::SocketAddr;

use socket2::SockRef;
use tokio::net::TcpStream;

/// DSCP Expedited Forwarding (RFC 3246).
const DSCP_EXPEDITED_FORWARDING: u8 = 46;

/// What a connection carries, for the mark of its packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrafficClass {
  /// Anything but voice alone: unmarked.
  #[default]
  BestEffort,
  /// Voice alone: Expedited Forwarding.
  Voice,
}

impl TrafficClass {
  /// The DSCP of the class (the upper six bits of the TOS / traffic class byte).
  pub fn dscp(self) -> u8 {
    match self {
      Self::BestEffort => 0,
      Self::Voice => DSCP_EXPEDITED_FORWARDING,
    }
  }
}

/// Marks the packets `stream` sends with `class`. The ECN bits stay the kernel's. On a platform
/// without the option for the stream's family (IPv6 on iOS and Windows) nothing is marked.
pub fn mark_tcp(stream: &TcpStream, class: TrafficClass) -> io::Result<()> {
  let tos = u32::from(class.dscp()) << 2;
  let socket = SockRef::from(stream);
  match stream.local_addr()? {
    SocketAddr::V4(_) => socket.set_tos_v4(tos),
    // A dual-stack socket's IPv4 peer (an IPv4-mapped address) goes out as IPv4: both options.
    SocketAddr::V6(local) => {
      if local.ip().to_ipv4_mapped().is_some() {
        socket.set_tos_v4(tos)?;
      }
      set_tclass_v6(&socket, tos)
    },
  }
}

#[cfg(any(
  target_os = "android",
  target_os = "linux",
  target_os = "macos",
  target_os = "freebsd"
))]
fn set_tclass_v6(socket: &SockRef<'_>, tclass: u32) -> io::Result<()> {
  socket.set_tclass_v6(tclass)
}

/// No `IPV6_TCLASS` through socket2 here: unmarked.
#[cfg(not(any(
  target_os = "android",
  target_os = "linux",
  target_os = "macos",
  target_os = "freebsd"
)))]
fn set_tclass_v6(_socket: &SockRef<'_>, _tclass: u32) -> io::Result<()> {
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  use tokio::net::TcpListener;

  #[test]
  fn voice_is_expedited_forwarding_and_the_rest_unmarked() {
    assert_eq!(TrafficClass::Voice.dscp(), 46);
    assert_eq!(TrafficClass::BestEffort.dscp(), 0);
  }

  #[tokio::test]
  async fn a_voice_connection_over_ipv4_is_marked() {
    let stream = connected("127.0.0.1:0").await;
    mark_tcp(&stream, TrafficClass::Voice).unwrap();
    assert_eq!(SockRef::from(&stream).tos_v4().unwrap(), 0xB8);
  }

  #[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd"
  ))]
  #[tokio::test]
  async fn a_voice_connection_over_ipv6_is_marked() {
    let Some(stream) = try_connected("[::1]:0").await else {
      // A runner without IPv6 loopback.
      return;
    };
    mark_tcp(&stream, TrafficClass::Voice).unwrap();
    assert_eq!(SockRef::from(&stream).tclass_v6().unwrap(), 0xB8);
  }

  #[tokio::test]
  async fn best_effort_stays_unmarked() {
    let stream = connected("127.0.0.1:0").await;
    mark_tcp(&stream, TrafficClass::BestEffort).unwrap();
    assert_eq!(SockRef::from(&stream).tos_v4().unwrap(), 0);
  }

  async fn connected(address: &str) -> TcpStream {
    try_connected(address).await.expect("loopback connects")
  }

  async fn try_connected(address: &str) -> Option<TcpStream> {
    let listener = TcpListener::bind(address).await.ok()?;
    let target = listener.local_addr().ok()?;
    let (stream, _) = tokio::join!(TcpStream::connect(target), listener.accept());
    stream.ok()
  }
}
