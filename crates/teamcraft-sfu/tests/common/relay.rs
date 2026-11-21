//! A relay between one client and the SFU whose directions can be limited like a bottleneck, for
//! the suite's tests of limited links. Each of the client's sockets gets an SFU-side socket of its
//! own, as a NAT maps each socket of a device on its own: the call's own voice takes a QUIC
//! connection of its own (#2141), from a socket of its own, through the same bottleneck. Packets packets leave at the link's rate, wait in a queue of at
//! most [`MAX_QUEUE_DELAY`] and are dropped when it is full. A direction can also lose packets in
//! bursts and delay them, both fluctuating as a real link does ([`Impairment`]).

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
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
/// How often an [`Impairment`]'s loss and delay take their next step.
const IMPAIRMENT_STEP: Duration = Duration::from_millis(500);
/// Loss in a burst: about every other packet while the link is bad.
const BURST_LOSS: f64 = 0.5;
/// Packets a burst lasts on average.
const BURST_PACKETS: f64 = 4.0;

/// Relays one client's UDP to the SFU and back; each direction can be limited like a bottleneck.
pub struct Relay {
  /// Where the client sends to instead of the SFU.
  pub address: SocketAddr,
  pub to_sfu: Shaper,
  pub to_client: Shaper,
}

/// Packets with the time they leave the bottleneck, and what they were tagged with (where to).
type Departures<T> = mpsc::UnboundedReceiver<(tokio::time::Instant, T, Vec<u8>)>;

/// One direction of a [`Relay`]: packets leave at the limit's rate after waiting in a queue of
/// at most [`MAX_QUEUE_DELAY`]; a packet that would wait longer is dropped.
#[derive(Clone)]
pub struct Shaper {
  /// Bit/s; 0 for no limit.
  limit_bps: Arc<AtomicU64>,
  start: Instant,
  /// Every packet: when it came (since the start), its bytes, and whether it was dropped.
  pub log: Arc<Mutex<Vec<(Duration, usize, bool)>>>,
  maybe_impairment: Arc<Mutex<Option<ImpairedLink>>>,
}

/// Loss and delay that fluctuate, seeded: every [`IMPAIRMENT_STEP`] the average loss walks within
/// `loss_percent` and the one-way delay within `delay`, by up to a quarter of each range. Losses
/// come in bursts (Gilbert–Elliott: a bad state of [`BURST_PACKETS`] packets on average that loses
/// [`BURST_LOSS`] of them), as on Wi-Fi or a mobile link, not one packet here and there. Packets
/// keep their order.
#[derive(Debug, Clone, Copy)]
pub struct Impairment {
  pub seed: u64,
  /// The average loss's range, in percent of the packets.
  pub loss_percent: (f64, f64),
  pub delay: (Duration, Duration),
  /// Packets smaller than this are never lost (zero: any may be): the voice and the transport's
  /// acknowledgements, for a test of what happens to the video alone.
  pub spared_below_bytes: usize,
}

/// An [`Impairment`] at work.
struct ImpairedLink {
  impairment: Impairment,
  rng: u64,
  /// When the loss and delay took their last step (since the start).
  stepped_at: Duration,
  loss: f64,
  delay: Duration,
  is_bad: bool,
  /// When the last packet leaves: none overtakes it.
  last_departure: tokio::time::Instant,
}

impl Shaper {
  fn new(start: Instant) -> Self {
    Self {
      limit_bps: Arc::default(),
      start,
      log: Arc::default(),
      maybe_impairment: Arc::default(),
    }
  }

  /// Loses and delays packets as `maybe_impairment` says from now on; `None` stops it.
  pub fn set_impairment(&self, maybe_impairment: Option<Impairment>) {
    *self.maybe_impairment.lock().unwrap() =
      maybe_impairment.map(|impairment| ImpairedLink::new(impairment, self.start.elapsed()));
  }

  /// Limits the direction to `maybe_limit_bps` bit/s; `None` lifts the limit.
  pub fn set_limit(&self, maybe_limit_bps: Option<u64>) {
    self
      .limit_bps
      .store(maybe_limit_bps.unwrap_or(0), Ordering::Relaxed);
  }

  /// The queue: tagged packets go into the sender, and come out of the receiver with the time
  /// they leave (dropped ones don't).
  fn spawn_queue<T: Send + 'static>(&self) -> (mpsc::UnboundedSender<(T, Vec<u8>)>, Departures<T>) {
    let (packets, mut incoming) = mpsc::unbounded_channel::<(T, Vec<u8>)>();
    let (departures, queue) = mpsc::unbounded_channel();
    let shaper = self.clone();
    tokio::spawn(async move {
      let mut next_free = tokio::time::Instant::now();
      while let Some((tag, packet)) = incoming.recv().await {
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
        let mut departure = departure;
        if let Some(link) = shaper.maybe_impairment.lock().unwrap().as_mut() {
          match link.pass(at, packet.len(), departure) {
            Some(delayed) => departure = delayed,
            None => {
              shaper.log.lock().unwrap().push((at, packet.len(), true));
              continue;
            },
          }
        }
        shaper.log.lock().unwrap().push((at, packet.len(), false));
        if departures.send((departure, tag, packet)).is_err() {
          return;
        }
      }
    });
    (packets, queue)
  }
}

impl ImpairedLink {
  fn new(impairment: Impairment, at: Duration) -> Self {
    let mut link = Self {
      impairment,
      rng: impairment.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
      stepped_at: at,
      loss: 0.0,
      delay: Duration::ZERO,
      is_bad: false,
      last_departure: tokio::time::Instant::now(),
    };
    let (low, high) = impairment.loss_percent;
    link.loss = (low + link.uniform() * (high - low)) / 100.0;
    let (shortest, longest) = impairment.delay;
    link.delay = shortest + (longest - shortest).mul_f64(link.uniform());
    link
  }

  /// When a packet that came at `at` and would leave at `departure` leaves, or `None` if it's lost.
  fn pass(
    &mut self,
    at: Duration,
    bytes: usize,
    departure: tokio::time::Instant,
  ) -> Option<tokio::time::Instant> {
    while at >= self.stepped_at + IMPAIRMENT_STEP {
      self.stepped_at += IMPAIRMENT_STEP;
      self.step();
    }
    // The bad state's share of the packets that gives the average loss: `loss / BURST_LOSS`.
    let bad_share = (self.loss / BURST_LOSS).min(0.9);
    let leave_bad = 1.0 / BURST_PACKETS;
    let enter_bad = bad_share * leave_bad / (1.0 - bad_share);
    let roll = self.uniform();
    self.is_bad = if self.is_bad {
      roll >= leave_bad
    } else {
      roll < enter_bad
    };
    if self.is_bad && self.uniform() < BURST_LOSS && bytes >= self.impairment.spared_below_bytes {
      return None;
    }
    let delayed = (departure + self.delay).max(self.last_departure);
    self.last_departure = delayed;
    Some(delayed)
  }

  /// The next step of the walk: loss and delay move by up to a quarter of their ranges.
  fn step(&mut self) {
    let (low, high) = self.impairment.loss_percent;
    let loss_step = (self.uniform() - 0.5) / 2.0 * (high - low) / 100.0;
    self.loss = (self.loss + loss_step).clamp(low / 100.0, high / 100.0);
    let (shortest, longest) = self.impairment.delay;
    let range = (longest - shortest).as_secs_f64();
    let delay_step = (self.uniform() - 0.5) / 2.0 * range;
    let delay = (self.delay.as_secs_f64() + delay_step).clamp(shortest.as_secs_f64(), longest.as_secs_f64());
    self.delay = Duration::from_secs_f64(delay);
  }

  /// Uniform in `[0, 1)` (xorshift).
  fn uniform(&mut self) -> f64 {
    self.rng ^= self.rng << 13;
    self.rng ^= self.rng >> 7;
    self.rng ^= self.rng << 17;
    (self.rng >> 11) as f64 / (1u64 << 53) as f64
  }
}

impl Relay {
  /// Relays to `sfu`, without a limit until one is set; logs are timed from `start`.
  pub fn start(sfu: SocketAddr, start: Instant) -> Self {
    let client_side = Arc::new(bind_socket());
    let address = client_side.local_addr().unwrap();
    let (to_sfu, to_client) = (Shaper::new(start), Shaper::new(start));
    let (into_sfu_queue, mut sfu_queue) = to_sfu.spawn_queue::<Arc<UdpSocket>>();
    let (into_client_queue, mut client_queue) = to_client.spawn_queue::<SocketAddr>();

    let mut mappings = ClientMappings::new(sfu);
    receive_on_thread(Arc::clone(&client_side), move |packet, from| {
      let sfu_side = mappings.sfu_side(from, |sfu_side| {
        let into_client_queue = into_client_queue.clone();
        receive_on_thread(sfu_side, move |packet, _| {
          let _ = into_client_queue.send((from, packet.to_vec()));
        });
      });
      // The queue ends with the test's runtime; this thread with the test's process.
      let _ = into_sfu_queue.send((sfu_side, packet.to_vec()));
    });
    tokio::spawn(async move {
      while let Some((departure, sfu_side, packet)) = sfu_queue.recv().await {
        tokio::time::sleep_until(departure).await;
        let _ = sfu_side.send(&packet);
      }
    });
    tokio::spawn(async move {
      while let Some((departure, client, packet)) = client_queue.recv().await {
        tokio::time::sleep_until(departure).await;
        let _ = client_side.send_to(&packet, client);
      }
    });
    Self {
      address,
      to_sfu,
      to_client,
    }
  }
}

/// Relays a client's UDP from `client_side` to `sfu` and back on the tokio runtime, each client
/// socket through an SFU-side socket of its own (a NAT's mappings: the own voice takes a connection
/// of its own, #2141). `upstream` and `downstream` see every packet of their direction and say
/// whether it passes.
pub fn relay_per_client(
  client_side: Arc<tokio::net::UdpSocket>,
  sfu: SocketAddr,
  mut upstream: impl FnMut(&[u8]) -> bool + Send + 'static,
  downstream: impl Fn(&[u8]) -> bool + Send + Sync + 'static,
) {
  let downstream = Arc::new(downstream);
  tokio::spawn(async move {
    let mut by_client: HashMap<SocketAddr, Arc<tokio::net::UdpSocket>> = HashMap::new();
    let mut buffer = vec![0; 65_536];
    while let Ok((len, from)) = client_side.recv_from(&mut buffer).await {
      let sfu_side = match by_client.get(&from) {
        Some(socket) => Arc::clone(socket),
        None => {
          let socket = Arc::new(
            tokio::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
              .await
              .unwrap(),
          );
          socket.connect(sfu).await.unwrap();
          let (reading, back, downstream) = (
            Arc::clone(&socket),
            Arc::clone(&client_side),
            Arc::clone(&downstream),
          );
          tokio::spawn(async move {
            let mut buffer = vec![0; 65_536];
            while let Ok(len) = reading.recv(&mut buffer).await {
              if downstream(&buffer[..len]) {
                let _ = back.send_to(&buffer[..len], from).await;
              }
            }
          });
          by_client.insert(from, Arc::clone(&socket));
          socket
        },
      };
      if upstream(&buffer[..len]) {
        let _ = sfu_side.send(&buffer[..len]).await;
      }
    }
  });
}

/// A relay's SFU-side sockets, one per client socket (a NAT's mappings).
pub struct ClientMappings {
  sfu: SocketAddr,
  by_client: HashMap<SocketAddr, Arc<UdpSocket>>,
  /// Every SFU-side socket so far, for [`dropped_by_kernel`].
  pub all: Arc<Mutex<Vec<Arc<UdpSocket>>>>,
}

impl ClientMappings {
  pub fn new(sfu: SocketAddr) -> Self {
    Self {
      sfu,
      by_client: HashMap::new(),
      all: Arc::default(),
    }
  }

  /// The SFU-side socket of `client`; a new one is handed to `on_new` first, to read what the SFU
  /// sends there.
  pub fn sfu_side(&mut self, client: SocketAddr, on_new: impl FnOnce(Arc<UdpSocket>)) -> Arc<UdpSocket> {
    if let Some(socket) = self.by_client.get(&client) {
      return Arc::clone(socket);
    }
    let socket = Arc::new(bind_socket());
    socket.connect(self.sfu).unwrap();
    self.all.lock().unwrap().push(Arc::clone(&socket));
    self.by_client.insert(client, Arc::clone(&socket));
    on_new(Arc::clone(&socket));
    socket
  }
}

/// The socket of a relay that a client sends to instead of the SFU.
pub fn bind_socket() -> UdpSocket {
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

/// Sends what goes into the returned queue (tagged with where to) at `rate_bps` from a thread of
/// its own, until the
/// queue is dropped: a link's rate instead of loopback's, where a keyframe arrives in one burst
/// and a socket whose buffer the system caps (`net.core.rmem_max`) overflows however idle the
/// machine is. No packet is dropped here.
pub fn paced_on_thread<T: Send + 'static>(
  rate_bps: u64,
  mut send: impl FnMut(&T, &[u8]) + Send + 'static,
) -> std_mpsc::Sender<(T, Vec<u8>)> {
  let (packets, queue) = std_mpsc::channel::<(T, Vec<u8>)>();
  std::thread::spawn(move || {
    let mut next_free = Instant::now();
    while let Ok((tag, packet)) = queue.recv() {
      let now = Instant::now();
      std::thread::sleep(next_free.saturating_duration_since(now));
      send(&tag, &packet);
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
