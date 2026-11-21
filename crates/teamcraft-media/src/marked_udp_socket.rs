//! A UDP socket for quinn whose datagrams leave with a DSCP (#2141): the call's own voice on a QUIC
//! connection of its own, marked Expedited Forwarding for the first hop's voice queue.
//!
//! quinn can't do it on its own socket: quinn-udp 0.5 hands every datagram's TOS / traffic class
//! byte to the kernel in a control message (`IP_TOS` / `IPV6_TCLASS`) that holds only the ECN
//! codepoint of the datagram, and that control message takes the place of whatever DSCP the socket
//! has. So this socket sends with plain `sendto` and no control message, after setting the socket's
//! byte to the class's DSCP above the datagram's ECN codepoint ([`TrafficClass::tos_byte`]): ECN
//! keeps working as quinn decides it, datagram by datagram. The byte changes rarely (quinn marks
//! every datagram ECT(0) while the path validates ECN, none after it failed), so the extra system
//! call is rare too. Receiving stays quinn-udp's, which reads the ECN codepoint of what arrives.
//!
//! What it gives up against quinn's own socket is fine for a connection of 50 small datagrams a
//! second: no segmentation offload (GSO), and no source address per datagram (the wildcard socket's
//! route picks it, as after any network change).
//!
//! Unix only (Linux, Android, macOS, iOS). Windows ignores `IP_TOS` from applications (it wants its
//! QoS API), so there the voice stays on the session's connection, unmarked.

use std::fmt;
use std::future::Future;
use std::io::{self, IoSliceMut};
use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use socket2::SockRef;
use teamcraft_net::traffic::{TrafficClass, set_udp_tos};
use tokio::io::Interest;
use tokio::net::UdpSocket;
use tracing::debug;
use wtransport::quinn::udp::{RecvMeta, Transmit, UdpSocketState};
use wtransport::quinn::{AsyncUdpSocket, UdpPoller};

/// A failing send is logged at most this often: UDP errors are no news to QUIC, which retransmits.
const SEND_ERROR_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// A quinn socket that marks what it sends with its [`TrafficClass`].
pub(crate) struct MarkedUdpSocket {
  io: UdpSocket,
  state: UdpSocketState,
  class: TrafficClass,
  /// Sends hold it from setting the byte to the datagram's leaving, so no datagram leaves with
  /// another's ECN codepoint.
  marking: Mutex<Marking>,
}

/// What the socket's byte is set to now.
#[derive(Default)]
struct Marking {
  /// The byte and the family it was set for; `None` before the first send, or after setting failed.
  maybe_applied: Option<(u8, IpFamily)>,
  maybe_failure_logged_at: Option<Instant>,
  maybe_send_error_logged_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IpFamily {
  V4,
  V6,
}

/// Waits for the socket to take datagrams again, for one of quinn's senders.
struct WritablePoller {
  socket: Arc<MarkedUdpSocket>,
  maybe_waiting: Option<Pin<Box<dyn Future<Output = io::Result<()>> + Send + Sync>>>,
}

impl MarkedUdpSocket {
  /// Takes `socket` (bound, not yet in use) for a call's connection of `class`. Needs the tokio
  /// runtime the connection runs on.
  pub(crate) fn new(socket: StdUdpSocket, class: TrafficClass) -> io::Result<Self> {
    let state = UdpSocketState::new((&socket).into())?;
    socket.set_nonblocking(true)?;
    Ok(Self {
      io: UdpSocket::from_std(socket)?,
      state,
      class,
      marking: Mutex::new(Marking::default()),
    })
  }

  /// Sets the socket's byte for `transmit`, unless it is set so already. A failure is logged now and
  /// then; the datagram then leaves with what the socket has (unmarked at worst).
  fn mark(&self, marking: &mut Marking, transmit: &Transmit<'_>) {
    let ecn_bits = transmit.ecn.map_or(0, |ecn| ecn as u8);
    let tos = self.class.tos_byte(ecn_bits);
    let family = IpFamily::of(transmit.destination);
    if marking.maybe_applied == Some((tos, family)) {
      return;
    }
    match set_udp_tos(SockRef::from(&self.io), transmit.destination, tos) {
      Ok(()) => marking.maybe_applied = Some((tos, family)),
      Err(error) => {
        marking.maybe_applied = None;
        if is_due(&mut marking.maybe_failure_logged_at) {
          debug!(%error, tos, "could not mark the voice connection's datagrams");
        }
      },
    }
  }

  /// Sends the datagram, or each of its segments. Errors other than a full buffer are dropped
  /// after a log line, as quinn's own socket does: QUIC recovers what got lost.
  fn send(&self, marking: &mut Marking, transmit: &Transmit<'_>) -> io::Result<()> {
    let segment_bytes = transmit
      .segment_size
      .filter(|size| *size > 0)
      .unwrap_or(transmit.contents.len())
      .max(1);
    for (index, segment) in transmit.contents.chunks(segment_bytes).enumerate() {
      match self.io.try_send_to(segment, transmit.destination) {
        Ok(_) => {},
        // Nothing went yet: quinn waits and offers the whole transmit again.
        Err(error) if error.kind() == io::ErrorKind::WouldBlock && index == 0 => return Err(error),
        Err(error) => {
          if is_due(&mut marking.maybe_send_error_logged_at) {
            debug!(%error, "a voice datagram could not be sent");
          }
          return Ok(());
        },
      }
    }
    Ok(())
  }
}

impl AsyncUdpSocket for MarkedUdpSocket {
  fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
    Box::pin(WritablePoller {
      socket: self,
      maybe_waiting: None,
    })
  }

  fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
    let mut marking = lock(&self.marking);
    self.mark(&mut marking, transmit);
    self.send(&mut marking, transmit)
  }

  fn poll_recv(
    &self,
    cx: &mut Context<'_>,
    bufs: &mut [IoSliceMut<'_>],
    meta: &mut [RecvMeta],
  ) -> Poll<io::Result<usize>> {
    loop {
      ready!(self.io.poll_recv_ready(cx))?;
      if let Ok(count) = self.io.try_io(Interest::READABLE, || {
        self.state.recv((&self.io).into(), bufs, meta)
      }) {
        return Poll::Ready(Ok(count));
      }
    }
  }

  fn local_addr(&self) -> io::Result<SocketAddr> {
    self.io.local_addr()
  }

  /// One datagram per send: `sendto` without segmentation offload.
  fn max_transmit_segments(&self) -> usize {
    1
  }

  fn max_receive_segments(&self) -> usize {
    self.state.gro_segments()
  }

  fn may_fragment(&self) -> bool {
    self.state.may_fragment()
  }
}

impl fmt::Debug for MarkedUdpSocket {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MarkedUdpSocket")
      .field("class", &self.class)
      .finish_non_exhaustive()
  }
}

impl IpFamily {
  /// IPv4-mapped addresses of a dual-stack socket go out as IPv4.
  fn of(destination: SocketAddr) -> Self {
    match destination {
      SocketAddr::V4(_) => Self::V4,
      SocketAddr::V6(address) if address.ip().to_ipv4_mapped().is_some() => Self::V4,
      SocketAddr::V6(_) => Self::V6,
    }
  }
}

impl UdpPoller for WritablePoller {
  fn poll_writable(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    let this = &mut *self;
    let waiting = this.maybe_waiting.get_or_insert_with(|| {
      let socket = Arc::clone(&this.socket);
      Box::pin(async move { socket.io.writable().await })
    });
    let result = ready!(waiting.as_mut().poll(cx));
    this.maybe_waiting = None;
    Poll::Ready(result)
  }
}

impl fmt::Debug for WritablePoller {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("WritablePoller").finish_non_exhaustive()
  }
}

/// Whether a log line is due, at most one per [`SEND_ERROR_LOG_INTERVAL`]; notes it as written.
fn is_due(maybe_logged_at: &mut Option<Instant>) -> bool {
  let now = Instant::now();
  if maybe_logged_at.is_some_and(|at| now.duration_since(at) < SEND_ERROR_LOG_INTERVAL) {
    return false;
  }
  *maybe_logged_at = Some(now);
  true
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  // Nothing spans a panic point while it is held.
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
  use std::net::{Ipv4Addr, Ipv6Addr};

  use wtransport::quinn::udp::EcnCodepoint;

  use super::*;

  const PAYLOAD: &[u8] = b"voice";

  #[tokio::test]
  async fn ipv4_datagrams_leave_expedited_with_quinns_ecn_codepoint() {
    let (socket, receiver) = pair(Ipv4Addr::LOCALHOST.into()).await.unwrap();
    let destination = receiver.io.local_addr().unwrap();
    socket
      .try_send(&transmit(destination, Some(EcnCodepoint::Ect0)))
      .unwrap();
    assert_eq!(SockRef::from(&socket.io).tos_v4().unwrap(), 0xBA);
    assert_eq!(received_ecn(&receiver).await, Some(EcnCodepoint::Ect0));

    socket.try_send(&transmit(destination, None)).unwrap();
    assert_eq!(SockRef::from(&socket.io).tos_v4().unwrap(), 0xB8, "Not-ECT");
    assert_eq!(received_ecn(&receiver).await, None);
  }

  #[cfg(any(target_os = "android", target_os = "linux", target_os = "macos"))]
  #[tokio::test]
  async fn ipv6_datagrams_leave_expedited_with_quinns_ecn_codepoint() {
    let Some((socket, receiver)) = pair(Ipv6Addr::LOCALHOST.into()).await else {
      // A runner without IPv6 loopback.
      return;
    };
    let destination = receiver.io.local_addr().unwrap();
    socket
      .try_send(&transmit(destination, Some(EcnCodepoint::Ect1)))
      .unwrap();
    assert_eq!(SockRef::from(&socket.io).tclass_v6().unwrap(), 0xB9);
    assert_eq!(received_ecn(&receiver).await, Some(EcnCodepoint::Ect1));
  }

  #[tokio::test]
  async fn every_segment_of_a_batch_is_sent() {
    let (socket, receiver) = pair(Ipv4Addr::LOCALHOST.into()).await.unwrap();
    let destination = receiver.io.local_addr().unwrap();
    let contents = [1u8, 1, 2, 2, 3];
    let batch = Transmit {
      destination,
      ecn: None,
      contents: &contents,
      segment_size: Some(2),
      src_ip: None,
    };
    socket.try_send(&batch).unwrap();
    let mut lengths = Vec::new();
    for _ in 0..3 {
      let mut buf = [0u8; 16];
      let (length, _) = tokio::time::timeout(Duration::from_secs(5), receiver.io.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();
      lengths.push(length);
    }
    assert_eq!(lengths, [2, 2, 1]);
  }

  #[test]
  fn ipv4_mapped_destinations_are_ipv4() {
    let mapped: SocketAddr = "[::ffff:192.0.2.1]:443".parse().unwrap();
    assert_eq!(IpFamily::of(mapped), IpFamily::V4);
    let native: SocketAddr = "[2001:db8::1]:443".parse().unwrap();
    assert_eq!(IpFamily::of(native), IpFamily::V6);
  }

  async fn pair(ip: std::net::IpAddr) -> Option<(MarkedUdpSocket, MarkedUdpSocket)> {
    let sender = StdUdpSocket::bind(SocketAddr::new(ip, 0)).ok()?;
    let receiver = StdUdpSocket::bind(SocketAddr::new(ip, 0)).ok()?;
    let socket = MarkedUdpSocket::new(sender, TrafficClass::Voice).ok()?;
    // As quinn does (`UdpPoller::poll_writable`) before it sends: tokio learns the socket is ready.
    socket.io.writable().await.ok()?;
    // quinn-udp's state turns on reading the ECN codepoint of what arrives.
    let receiver = MarkedUdpSocket::new(receiver, TrafficClass::BestEffort).ok()?;
    Some((socket, receiver))
  }

  fn transmit(destination: SocketAddr, ecn: Option<EcnCodepoint>) -> Transmit<'static> {
    Transmit {
      destination,
      ecn,
      contents: PAYLOAD,
      segment_size: None,
      src_ip: None,
    }
  }

  /// The ECN codepoint the next datagram arrived with, as quinn reads it.
  async fn received_ecn(receiver: &MarkedUdpSocket) -> Option<EcnCodepoint> {
    let mut buf = [0u8; 2048];
    let mut meta = [RecvMeta::default()];
    let receiving = std::future::poll_fn(|cx| {
      let mut bufs = [IoSliceMut::new(&mut buf)];
      receiver.poll_recv(cx, &mut bufs, &mut meta)
    });
    let count = tokio::time::timeout(Duration::from_secs(5), receiving)
      .await
      .expect("a datagram in time")
      .unwrap();
    assert_eq!(count, 1);
    assert_eq!(&buf[..meta[0].len], PAYLOAD);
    meta[0].ecn
  }
}
