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
/// The ECN field: the low two bits of the TOS / traffic class byte (RFC 3168).
const ECN_MASK: u8 = 0b11;

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

  /// The whole TOS / traffic class byte of a datagram of this class: the DSCP above the ECN
  /// codepoint `ecn_bits` (of which only the low two bits count). A QUIC connection chooses the ECN
  /// codepoint per datagram (RFC 9000 §13.4); the class keeps the DSCP.
  pub fn tos_byte(self, ecn_bits: u8) -> u8 {
    (self.dscp() << 2) | (ecn_bits & ECN_MASK)
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

/// Sets the whole TOS / traffic class byte (DSCP and ECN, [`TrafficClass::tos_byte`]) of what the
/// UDP `socket` sends to `destination` from now on, for a sender that writes no per-datagram TOS of
/// its own. IPv4 destinations, IPv4-mapped ones of a dual-stack socket included, take `IP_TOS`;
/// IPv6 ones `IPV6_TCLASS`, which is a no-op where socket2 lacks it (iOS, Windows).
pub fn set_udp_tos(socket: SockRef<'_>, destination: SocketAddr, tos: u8) -> io::Result<()> {
  let tos = u32::from(tos);
  let is_ipv4 = match destination {
    SocketAddr::V4(_) => true,
    SocketAddr::V6(address) => address.ip().to_ipv4_mapped().is_some(),
  };
  if is_ipv4 {
    socket.set_tos_v4(tos)
  } else {
    set_tclass_v6(&socket, tos)
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

  #[test]
  fn the_tos_byte_keeps_the_ecn_codepoint_below_the_dscp() {
    // Not-ECT, ECT(1), ECT(0), CE.
    assert_eq!(TrafficClass::Voice.tos_byte(0b00), 0xB8);
    assert_eq!(TrafficClass::Voice.tos_byte(0b01), 0xB9);
    assert_eq!(TrafficClass::Voice.tos_byte(0b10), 0xBA);
    assert_eq!(TrafficClass::Voice.tos_byte(0b11), 0xBB);
    assert_eq!(TrafficClass::BestEffort.tos_byte(0b10), 0b10);
  }

  #[test]
  fn bits_above_the_ecn_field_cannot_change_the_dscp() {
    assert_eq!(TrafficClass::Voice.tos_byte(0xFE), 0xBA);
    assert_eq!(TrafficClass::BestEffort.tos_byte(0xFC), 0);
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

  #[test]
  fn a_udp_socket_takes_the_whole_byte_for_ipv4() {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let destination: SocketAddr = "127.0.0.1:9".parse().unwrap();
    set_udp_tos(
      SockRef::from(&socket),
      destination,
      TrafficClass::Voice.tos_byte(0b10),
    )
    .unwrap();
    assert_eq!(SockRef::from(&socket).tos_v4().unwrap(), 0xBA);
  }

  /// Linux takes `IP_TOS` on an IPv6 socket for its IPv4-mapped peers; other systems may refuse it,
  /// and the caller then sends unmarked.
  #[cfg(any(target_os = "android", target_os = "linux"))]
  #[test]
  fn a_dual_stack_udp_socket_marks_by_the_destinations_family() {
    let Ok(socket) = std::net::UdpSocket::bind("[::]:0") else {
      // A runner without IPv6.
      return;
    };
    let mapped: SocketAddr = "[::ffff:127.0.0.1]:9".parse().unwrap();
    set_udp_tos(SockRef::from(&socket), mapped, 0xBA).unwrap();
    assert_eq!(SockRef::from(&socket).tos_v4().unwrap(), 0xBA);
    let native: SocketAddr = "[::1]:9".parse().unwrap();
    set_udp_tos(SockRef::from(&socket), native, 0xB9).unwrap();
    assert_eq!(SockRef::from(&socket).tclass_v6().unwrap(), 0xB9);
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
