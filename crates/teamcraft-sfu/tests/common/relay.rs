//! A relay between one client and the SFU whose directions can be limited like a bottleneck, for
//! the suite's tests of limited links: packets leave at the link's rate, wait in a queue of at
//! most [`MAX_QUEUE_DELAY`] and are dropped when it is full.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// The bottleneck's queue: a packet that would wait longer is dropped (a home router's buffer).
pub const MAX_QUEUE_DELAY: Duration = Duration::from_millis(250);

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
  pub async fn start(sfu: SocketAddr, start: Instant) -> Self {
    let client_side = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    let sfu_side = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    sfu_side.connect(sfu).await.unwrap();
    let address = client_side.local_addr().unwrap();
    let (to_sfu, to_client) = (Shaper::new(start), Shaper::new(start));
    let client: Arc<Mutex<Option<SocketAddr>>> = Arc::default();

    let (into_sfu_queue, mut sfu_queue) = to_sfu.spawn_queue();
    let (from_client, known_client) = (Arc::clone(&client_side), Arc::clone(&client));
    tokio::spawn(async move {
      let mut buffer = vec![0; 65_536];
      while let Ok((len, from)) = from_client.recv_from(&mut buffer).await {
        *known_client.lock().unwrap() = Some(from);
        if into_sfu_queue.send(buffer[..len].to_vec()).is_err() {
          return;
        }
      }
    });
    let towards_sfu = Arc::clone(&sfu_side);
    tokio::spawn(async move {
      while let Some((departure, packet)) = sfu_queue.recv().await {
        tokio::time::sleep_until(departure).await;
        let _ = towards_sfu.send(&packet).await;
      }
    });

    let (into_client_queue, mut client_queue) = to_client.spawn_queue();
    tokio::spawn(async move {
      let mut buffer = vec![0; 65_536];
      while let Ok(len) = sfu_side.recv(&mut buffer).await {
        if into_client_queue.send(buffer[..len].to_vec()).is_err() {
          return;
        }
      }
    });
    tokio::spawn(async move {
      while let Some((departure, packet)) = client_queue.recv().await {
        tokio::time::sleep_until(departure).await;
        let maybe_client = *client.lock().unwrap();
        if let Some(client) = maybe_client {
          let _ = client_side.send_to(&packet, client).await;
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
