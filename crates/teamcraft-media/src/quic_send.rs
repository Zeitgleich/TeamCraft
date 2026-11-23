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
//!
//! QUIC's rate is its window per round trip, and both are only learnt from acknowledgements: when
//! an upload collapses, they come back through a bottleneck queue of seconds, and until then QUIC
//! goes by the old rate. So while QUIC holds a backlog, what it drained from it lately is its rate
//! too ([`DrainMeter`], the lower of the two counts): without it, QUIC took in seconds of the
//! camera at the collapsed rate, which went out after the camera had stopped for the voice and
//! held it back for 4–5 s (#1889). What QUIC holds can't be taken back; what waits here can.
//!
//! The direct path of a 1:1 call (#127) sends its video the same way, on its own QUIC connection
//! ([`QuicSender::spawn_direct`]): its voice, sent past the queue, waits behind at most
//! [`MAX_QUIC_WAIT`] of video in QUIC.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;
use tracing::debug;
use web_time::Instant;
use wtransport::Connection;
use wtransport::quinn::{self, TransportConfig};

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
  connection: Link,
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
  drain: DrainMeter,
}

/// The connection a [`QuicSender`] sends on.
enum Link {
  /// A WebTransport session's datagrams (to or from the SFU).
  Session(Connection),
  /// A plain QUIC connection's (the direct path, #127).
  Direct(quinn::Connection),
}

/// How fast QUIC drains what it holds, while it holds something (see the module docs). Pure: time
/// and the bytes QUIC holds are passed in.
#[derive(Debug, Default)]
pub struct DrainMeter {
  /// The last reading: when, and how many bytes QUIC held then.
  maybe_last: Option<(Instant, usize)>,
  /// Bytes handed to QUIC since then.
  handed: usize,
}

impl QuicSender {
  /// Starts the task that hands queued datagrams to `connection` (on the current tokio runtime).
  pub fn spawn(connection: Connection, config: SendQueueConfig) -> Self {
    Self::spawn_on(Link::Session(connection), config)
  }

  /// The same for the direct path's QUIC connection of a 1:1 call (#127).
  pub fn spawn_direct(connection: quinn::Connection, config: SendQueueConfig) -> Self {
    Self::spawn_on(Link::Direct(connection), config)
  }

  fn spawn_on(connection: Link, config: SendQueueConfig) -> Self {
    let shared = Arc::new(Shared {
      connection,
      state: Mutex::new(State {
        queue: SendQueue::new(config),
        maybe_allowance: None,
        drain: DrainMeter::default(),
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
      self.shared.send(&mut state, datagram);
      return;
    }
    state.queue.push(datagram.to_vec(), now);
    drop(state);
    self.shared.wake.notify_one();
  }

  /// Drops the video that waits in the queue (`SendQueue::drop_video`): the own video stopped
  /// for the voice (#1889). What QUIC holds already goes out.
  pub fn drop_queued_video(&self) {
    lock(&self.shared.state).queue.drop_video(Instant::now());
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
    let quic = self.connection.quic();
    let space = quic.datagram_send_buffer_space();
    let held = QUIC_DATAGRAM_BUFFER_BYTES.saturating_sub(space + QUINN_DATAGRAM_OVERHEAD);
    let allowance = match state.maybe_allowance {
      Some((allowance, at)) if now.saturating_duration_since(at) < RATE_REFRESH => allowance,
      _ => {
        let path = quic.stats().path;
        let window_rate = path.cwnd as f64 / path.rtt.max(MIN_ROUND_TRIP).as_secs_f64();
        let bytes_per_second = match state.drain.on_reading(held, now) {
          Some(drained) => window_rate.min(drained),
          None => window_rate,
        };
        // Saturating: a float this large converts to `usize::MAX`.
        let allowance = ((bytes_per_second * MAX_QUIC_WAIT.as_secs_f64()) as usize).max(QUIC_BACKLOG_BYTES);
        state.maybe_allowance = Some((allowance, now));
        allowance
      },
    };
    held < allowance
  }

  fn send(&self, state: &mut State, datagram: &[u8]) {
    state.drain.on_handed(datagram.len());
    let result = match &self.connection {
      Link::Session(connection) => connection
        .send_datagram(datagram)
        .map_err(|error| error.to_string()),
      Link::Direct(connection) => connection
        .send_datagram(datagram.to_vec().into())
        .map_err(|error| error.to_string()),
    };
    if let Err(error) = result {
      debug!(%error, "datagram dropped");
    }
  }
}

impl Link {
  fn quic(&self) -> &quinn::Connection {
    match self {
      Self::Session(connection) => connection.quic_connection(),
      Self::Direct(connection) => connection,
    }
  }
}

impl DrainMeter {
  /// `bytes` were handed to QUIC.
  pub fn on_handed(&mut self, bytes: usize) {
    self.handed = self.handed.saturating_add(bytes);
  }

  /// QUIC holds `held` bytes at `now`: how many bytes per second it drained since the last
  /// reading, if it held a backlog all along (otherwise it sent all it had, and that says nothing
  /// about its rate).
  pub fn on_reading(&mut self, held: usize, now: Instant) -> Option<f64> {
    let maybe_last = self.maybe_last.replace((now, held));
    let handed = std::mem::take(&mut self.handed);
    let (at, last_held) = maybe_last?;
    let elapsed = now.saturating_duration_since(at);
    if last_held == 0 || held == 0 || elapsed.is_zero() {
      return None;
    }
    let drained = (last_held + handed).saturating_sub(held);
    Some(drained as f64 / elapsed.as_secs_f64())
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
          shared.send(&mut state, &datagram);
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

#[cfg(test)]
mod tests {
  use super::*;

  fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
  }

  #[test]
  fn a_backlog_is_drained_at_what_left_it() {
    let start = Instant::now();
    let mut meter = DrainMeter::default();
    assert_eq!(meter.on_reading(30_000, start), None);
    meter.on_handed(2_000);
    // 30 kB held, 2 kB more handed, 31 kB held 10 ms later: 1 kB left, 100 kB/s.
    let rate = meter.on_reading(31_000, start + ms(10)).unwrap();
    assert!((rate - 100_000.0).abs() < 1.0, "{rate}");
  }

  #[test]
  fn a_queue_that_ran_empty_says_nothing_about_the_rate() {
    let start = Instant::now();
    let mut meter = DrainMeter::default();
    assert_eq!(meter.on_reading(0, start), None);
    meter.on_handed(5_000);
    // QUIC held nothing at the start: what it sent is what it was given.
    assert_eq!(meter.on_reading(1_000, start + ms(10)), None);
    meter.on_handed(1_000);
    // It held a backlog, then sent all of it.
    assert_eq!(meter.on_reading(0, start + ms(20)), None);
  }

  #[test]
  fn a_stalled_quic_drains_nothing() {
    let start = Instant::now();
    let mut meter = DrainMeter::default();
    meter.on_reading(40_000, start);
    meter.on_handed(1_200);
    assert_eq!(meter.on_reading(41_200, start + ms(10)), Some(0.0));
  }
}
