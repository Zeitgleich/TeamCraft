//! A relay between one client and the SFU whose directions can be limited like a bottleneck, for
//! the suite's tests of limited links: packets leave at the link's rate, wait in a queue of at
//! most [`MAX_QUEUE_DELAY`] and are dropped when it is full.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::sync::mpsc;

/// The bottleneck's queue: a packet that would wait longer is dropped (a home router's buffer).
pub const MAX_QUEUE_DELAY: Duration = Duration::from_millis(250);
/// A relay socket's receive buffer: some keyframes of a 1440p share.
const RECEIVE_BUFFER_BYTES: usize = 4 * 1024 * 1024;

/// Relays one client's UDP to the SFU and back; each direction can be limited like a bottleneck.
pub struct Relay {
  /// Where the client sends to instead of the SFU.
  pub address: SocketAddr,
  pub to_sfu: Shaper,
  pub to_client: Shaper,
}

/// Packets with the time they leave the bottleneck.
type Departures = mpsc::UnboundedReceiver<(tokio::time::Instant, Vec<u8>)>;

/// One direction of a [`Relay`]: packets leave at the limit's rate after waiting in a queue of
/// at most [`MAX_QUEUE_DELAY`]; a packet that would wait longer is dropped.
#[derive(Clone)]
pub struct Shaper {
  /// Bit/s; 0 for no limit.
  limit_bps: Arc<AtomicU64>,
  start: Instant,
  /// Every packet: when it came (since the start), its bytes, and whether it was dropped.
  pub log: Arc<Mutex<Vec<(Duration, usize, bool)>>>,
}

impl Shaper {
  fn new(start: Instant) -> Self {
    Self {
      limit_bps: Arc::default(),
      start,
      log: Arc::default(),
    }
  }

  /// Limits the direction to `maybe_limit_bps` bit/s; `None` lifts the limit.
  pub fn set_limit(&self, maybe_limit_bps: Option<u64>) {
    self
      .limit_bps
      .store(maybe_limit_bps.unwrap_or(0), Ordering::Relaxed);
  }

  /// The queue: packets go into the sender, and come out of the receiver with the time they
  /// leave (dropped ones don't).
  fn spawn_queue(&self) -> (mpsc::UnboundedSender<Vec<u8>>, Departures) {
    let (packets, mut incoming) = mpsc::unbounded_channel::<Vec<u8>>();
    let (departures, queue) = mpsc::unbounded_channel();
    let shaper = self.clone();
    tokio::spawn(async move {
      let mut next_free = tokio::time::Instant::now();
      while let Some(packet) = incoming.recv().await {
        let now = tokio::time::Instant::now();
        let at = shaper.start.elapsed();
        let limit_bps = shaper.limit_bps.load(Ordering::Relaxed);
        let departure = next_free.max(now);
        if limit_bps > 0 && departure.saturating_duration_since(now) > MAX_QUEUE_DELAY {
          shaper.log.lock().unwrap().push((at, packet.len(), true));
          continue;
        }
        if limit_bps > 0 {
          next_free = departure + Duration::from_secs_f64(packet.len() as f64 * 8.0 / limit_bps as f64);
        } else {
          next_free = departure;
        }
        shaper.log.lock().unwrap().push((at, packet.len(), false));
        if departures.send((departure, packet)).is_err() {
          return;
        }
      }
    });
    (packets, queue)
  }
}

impl Relay {
  /// Relays to `sfu`, without a limit until one is set; logs are timed from `start`.
  pub fn start(sfu: SocketAddr, start: Instant) -> Self {
    let (client_side, sfu_side) = bind_sockets(sfu);
    let address = client_side.local_addr().unwrap();
    let (to_sfu, to_client) = (Shaper::new(start), Shaper::new(start));
    let client: Arc<Mutex<Option<SocketAddr>>> = Arc::default();

    let (into_sfu_queue, mut sfu_queue) = to_sfu.spawn_queue();
    let known_client = Arc::clone(&client);
    receive_on_thread(Arc::clone(&client_side), move |packet, from| {
      *known_client.lock().unwrap() = Some(from);
      // The queue ends with the test's runtime; this thread with the test's process.
      let _ = into_sfu_queue.send(packet.to_vec());
    });
    let towards_sfu = Arc::clone(&sfu_side);
    tokio::spawn(async move {
      while let Some((departure, packet)) = sfu_queue.recv().await {
        tokio::time::sleep_until(departure).await;
        let _ = towards_sfu.send(&packet);
      }
    });

    let (into_client_queue, mut client_queue) = to_client.spawn_queue();
    receive_on_thread(sfu_side, move |packet, _| {
      let _ = into_client_queue.send(packet.to_vec());
    });
    tokio::spawn(async move {
      while let Some((departure, packet)) = client_queue.recv().await {
        tokio::time::sleep_until(departure).await;
        let maybe_client = *client.lock().unwrap();
        if let Some(client) = maybe_client {
          let _ = client_side.send_to(&packet, client);
        }
      }
    });
    Self {
      address,
      to_sfu,
      to_client,
    }
  }
}

/// The sockets of a relay to `sfu`: the one a client sends to, and the one connected to the SFU.
pub fn bind_sockets(sfu: SocketAddr) -> (Arc<UdpSocket>, Arc<UdpSocket>) {
  let client_side = bind_socket();
  let sfu_side = bind_socket();
  sfu_side.connect(sfu).unwrap();
  (Arc::new(client_side), Arc::new(sfu_side))
}

fn bind_socket() -> UdpSocket {
  let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP)).unwrap();
  // As much room as the system gives (it caps the size silently): see `receive_on_thread`.
  socket.set_recv_buffer_size(RECEIVE_BUFFER_BYTES).unwrap();
  socket
    .bind(&SocketAddr::from((Ipv6Addr::LOCALHOST, 0)).into())
    .unwrap();
  socket.into()
}

/// Hands `socket`'s packets to `on_packet` from a thread of its own, until the process ends.
///
/// A relay is the test's stand-in for a network, which never loses a packet for lack of CPU. As a
/// task of the runtime that also runs the SFU and the clients it would: a 1440p keyframe (half a
/// megabyte) overflows a socket's default buffer while the task waits for its turn, one lost packet
/// loses the keyframe, and the next keyframe meets the same fate (#1139). [`dropped_by_kernel`]
/// tells whether it happened anyway.
pub fn receive_on_thread(
  socket: Arc<UdpSocket>,
  mut on_packet: impl FnMut(&[u8], SocketAddr) + Send + 'static,
) {
  std::thread::spawn(move || {
    let mut buffer = vec![0; 65_536];
    while let Ok((len, from)) = socket.recv_from(&mut buffer) {
      on_packet(&buffer[..len], from);
    }
  });
}

/// Sends what goes into the returned queue at `rate_bps` from a thread of its own, until the
/// queue is dropped: a link's rate instead of loopback's, where a keyframe arrives in one burst
/// and a socket whose buffer the system caps (`net.core.rmem_max`) overflows however idle the
/// machine is. No packet is dropped here.
pub fn paced_on_thread(
  rate_bps: u64,
  mut send: impl FnMut(&[u8]) + Send + 'static,
) -> std_mpsc::Sender<Vec<u8>> {
  let (packets, queue) = std_mpsc::channel::<Vec<u8>>();
  std::thread::spawn(move || {
    let mut next_free = Instant::now();
    while let Ok(packet) = queue.recv() {
      let now = Instant::now();
      std::thread::sleep(next_free.saturating_duration_since(now));
      send(&packet);
      next_free = next_free.max(now) + Duration::from_secs_f64(packet.len() as f64 * 8.0 / rate_bps as f64);
    }
  });
  packets
}

/// What the system allows a socket's receive buffer to be (`net.core.rmem_max`), where it tells.
pub fn receive_buffer_cap_bytes() -> Option<u64> {
  std::fs::read_to_string("/proc/sys/net/core/rmem_max")
    .ok()?
    .trim()
    .parse()
    .ok()
}

/// What the kernel dropped for lack of room at all of this network namespace's UDP sockets: the
/// SFU's and the clients' too. A test that runs alone has them to itself.
pub fn dropped_by_kernel_everywhere() -> Option<u64> {
  let table = std::fs::read_to_string("/proc/net/udp6").ok()?;
  Some(
    table
      .lines()
      .skip(1)
      .filter_map(|line| line.split_whitespace().last()?.parse::<u64>().ok())
      .sum(),
  )
}

/// What the kernel dropped at `sockets` for lack of room: a test's bottleneck that is no network's.
/// `None` where the system doesn't tell (Linux does).
pub fn dropped_by_kernel(sockets: &[&UdpSocket]) -> Option<u64> {
  let table = std::fs::read_to_string("/proc/net/udp6").ok()?;
  let ports: Vec<u16> = sockets
    .iter()
    .map(|socket| socket.local_addr().unwrap().port())
    .collect();
  let dropped = table
    .lines()
    .skip(1)
    .filter_map(|line| {
      let fields: Vec<&str> = line.split_whitespace().collect();
      let port = u16::from_str_radix(fields.get(1)?.rsplit(':').next()?, 16).ok()?;
      ports
        .contains(&port)
        .then(|| fields.last()?.parse::<u64>().ok())
        .flatten()
    })
    .sum();
  Some(dropped)
}
