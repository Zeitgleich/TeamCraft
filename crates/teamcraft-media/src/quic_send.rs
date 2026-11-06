//! A call's media datagrams on its QUIC connection, through a [`SendQueue`] (#1218): shared by the
//! native clients (their uplink) and the SFU (each receiver's downlink).
//!
//! QUIC keeps what its congestion window doesn't let out in its datagram buffer, oldest first, and
//! what is there goes out whatever comes after it. So QUIC gets a datagram only while it holds less
//! than [`MAX_QUIC_WAIT`] of sending at its current rate (its congestion window per round trip),
//! and never less than [`QUIC_BACKLOG_BYTES`]. Otherwise the datagram waits in the [`SendQueue`],
//! which drops what goes stale and what a keyframe makes obsolete, and a task of the connection
//! hands it over once QUIC caught up. On a fast link a keyframe's burst goes to QUIC at once, as
//! before; once QUIC's window collapsed on a narrow one, it holds a few packets, not seconds of the
//! old layer.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;
use tracing::debug;
use web_time::Instant;
use wtransport::Connection;
use wtransport::quinn::TransportConfig;

use crate::send_queue::{QUIC_BACKLOG_BYTES, SendQueue, SendQueueConfig};

/// QUIC's datagram buffer (quinn's default): room for a keyframe's burst on a fast link. How much
/// of it is used is up to [`QuicSender`].
pub const QUIC_DATAGRAM_BUFFER_BYTES: usize = 1024 * 1024;
/// How long what QUIC holds may take it to send at its current rate. At 20 ms a loaded machine's
/// QUIC (its round trip stretched by the scheduler) took a 1440p share's keyframe a few packets at
/// a time, which looked like a queue to the presenter's estimator: the sharp layer was cut now and
/// then (`screen_sharpness`, four copies of the suite in parallel).
pub const MAX_QUIC_WAIT: Duration = Duration::from_millis(50);
/// QUIC's rate is read again after this long (it changes once per round trip at most).
const RATE_REFRESH: Duration = Duration::from_millis(10);
/// A round trip shorter than this (loopback) counts as this long: the rate is plenty either way.
const MIN_ROUND_TRIP: Duration = Duration::from_millis(1);
/// While datagrams wait for QUIC to catch up, it is checked this often (QUIC tells nothing when it
/// sent some, only when its whole buffer had been full).
const RECHECK_INTERVAL: Duration = Duration::from_millis(5);
/// What quinn counts for each datagram in its buffer besides the bytes it sends.
const QUINN_DATAGRAM_OVERHEAD: usize = 32;

/// Sets QUIC's datagram buffer of a call connection to [`QUIC_DATAGRAM_BUFFER_BYTES`].
pub fn set_datagram_send_buffer(transport: &mut TransportConfig) {
  transport.datagram_send_buffer_size(QUIC_DATAGRAM_BUFFER_BYTES);
}

/// Sends a connection's datagrams through a [`SendQueue`]. Dropping it stops its task.
pub struct QuicSender {
  shared: Arc<Shared>,
}

struct Shared {
  connection: Connection,
  state: Mutex<State>,
  /// A datagram was queued.
  wake: Notify,
  /// The sender was dropped.
  stop: Notify,
}

struct State {
  queue: SendQueue,
  /// How much QUIC may hold ([`MAX_QUIC_WAIT`] at its rate), and when that was worked out.
  maybe_allowance: Option<(usize, Instant)>,
}

impl QuicSender {
  /// Starts the task that hands queued datagrams to `connection` (on the current tokio runtime).
  pub fn spawn(connection: Connection, config: SendQueueConfig) -> Self {
    let shared = Arc::new(Shared {
      connection,
      state: Mutex::new(State {
        queue: SendQueue::new(config),
        maybe_allowance: None,
      }),
      wake: Notify::new(),
      stop: Notify::new(),
    });
    tokio::spawn(hand_over(Arc::clone(&shared)));
    Self { shared }
  }

  /// Sends one datagram, or queues it. Unreliable by design: what waits too long is dropped.
  pub fn send_datagram(&self, datagram: &[u8]) {
    let now = Instant::now();
    let mut state = lock(&self.shared.state);
    if state.queue.is_empty() && self.shared.is_keeping_up(&mut state, now) {
      // Nothing waits and QUIC keeps up: as before, without the task.
      self.shared.send(datagram);
      return;
    }
    state.queue.push(datagram.to_vec(), now);
    drop(state);
    self.shared.wake.notify_one();
  }

  /// Datagrams the queue dropped so far.
  pub fn dropped_packets(&self) -> u64 {
    lock(&self.shared.state).queue.dropped_packets()
  }
}

impl Drop for QuicSender {
  fn drop(&mut self) {
    self.shared.stop.notify_one();
  }
}

impl Shared {
  /// Whether QUIC takes more now: it holds less than its allowance.
  fn is_keeping_up(&self, state: &mut State, now: Instant) -> bool {
    let quic = self.connection.quic_connection();
    let allowance = match state.maybe_allowance {
      Some((allowance, at)) if now.saturating_duration_since(at) < RATE_REFRESH => allowance,
      _ => {
        let path = quic.stats().path;
        let bytes_per_second = path.cwnd as f64 / path.rtt.max(MIN_ROUND_TRIP).as_secs_f64();
        // Saturating: a float this large converts to `usize::MAX`.
        let allowance = ((bytes_per_second * MAX_QUIC_WAIT.as_secs_f64()) as usize).max(QUIC_BACKLOG_BYTES);
        state.maybe_allowance = Some((allowance, now));
        allowance
      },
    };
    let space = quic.datagram_send_buffer_space();
    let held = QUIC_DATAGRAM_BUFFER_BYTES.saturating_sub(space + QUINN_DATAGRAM_OVERHEAD);
    held < allowance
  }

  fn send(&self, datagram: &[u8]) {
    if let Err(error) = self.connection.send_datagram(datagram) {
      debug!(%error, "datagram dropped");
    }
  }
}

/// Hands queued datagrams to QUIC as it keeps up, until the sender is dropped.
async fn hand_over(shared: Arc<Shared>) {
  loop {
    let is_waiting = {
      let now = Instant::now();
      let mut state = lock(&shared.state);
      while !state.queue.is_empty() && shared.is_keeping_up(&mut state, now) {
        if let Some(datagram) = state.queue.pop(now) {
          shared.send(&datagram);
        }
      }
      !state.queue.is_empty()
    };
    if is_waiting {
      tokio::select! {
        () = tokio::time::sleep(RECHECK_INTERVAL) => {},
        () = shared.stop.notified() => return,
      }
    } else {
      tokio::select! {
        () = shared.wake.notified() => {},
        () = shared.stop.notified() => return,
      }
    }
  }
}

fn lock(mutex: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
  // A panicked task leaves a queue that is still consistent enough to drop.
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
