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
//! A sender's uplink also paces its video ([`QuicSender::pace_video_for`], #2218): QUIC sends a
//! picture as fast as its window allows, which after a quiet while is far faster than the path.
//! A still screen's slide or a keyframe of its sharp layer (100–180 kB) then reached a home
//! router's queue at once and lost its tail there, on an uplink with room for the share: the
//! repair asked for it again, the keyframes asked for after it overflowed the same queue (#2246),
//! and the voice next to it was lost in the same queue, so the video was held for it. Paced at
//! [`PACING_FACTOR`] times the uplink estimate the picture spreads over a few hundred
//! milliseconds, which a queue of a quarter second holds. The pacer never holds video longer than
//! [`MAX_PACING_DELAY`] (it speeds up for a backlog), so an estimate that is too low doesn't hold
//! a picture back for long; voice and padding (the probes' own rates) go unpaced. The datagrams
//! the pacer held are reported with the time they left ([`QuicSender::take_departures`]): the
//! sender's estimator measures their delay from then, not from their numbering, or the pacer's
//! own wait looks like a queue on the path.
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

use crate::datagram::{DatagramHeader, TransportSeq};
use crate::send_queue::{QUIC_BACKLOG_BYTES, SendQueue, SendQueueConfig, is_video_frame};

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
/// The rate video is paced at on a sender's uplink, as a multiple of its estimate: enough room
/// above it for the estimate's growth and a picture's burst, little enough that a keyframe of
/// three times the queue's worth of a 250 ms queue arrives within it (libwebrtc paces at 2.5).
pub const PACING_FACTOR: f64 = 2.0;
/// The longest the pacer holds the video waiting: it sends faster than its rate when more waits.
/// Below the send queue's wait ([`SendQueueConfig::max_wait`]), so a frame doesn't go stale
/// waiting for the pacer.
pub const MAX_PACING_DELAY: Duration = Duration::from_millis(200);
/// What the pacer lets out at once after a pause: a few datagrams.
const PACING_BURST_BYTES: f64 = 5_000.0;
/// The most departures kept until they are taken ([`QuicSender::take_departures`]): seconds of a
/// share's keyframes.
const MAX_DEPARTURES: usize = 4096;
/// The least time left that the pacer spreads what waits over, once it waited about
/// [`MAX_PACING_DELAY`]: the rest goes out within a few of the sender's checks.
const MIN_PACING_LEFT: Duration = Duration::from_millis(10);

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
  pacer: Pacer,
  /// Paced datagrams that went out from the queue: their transport sequence numbers and when.
  departures: Vec<(TransportSeq, Instant)>,
}

/// The connection a [`QuicSender`] sends on.
enum Link {
  /// A WebTransport session's datagrams (to or from the SFU).
  Session(Connection),
  /// A plain QUIC connection's (the direct path, #127).
  Direct(quinn::Connection),
}

/// Lets video out at a rate (see the module docs). Pure: time and what waits are passed in.
#[derive(Debug, Default)]
pub struct Pacer {
  /// Bytes per second; `None`: unpaced.
  maybe_rate: Option<f64>,
  /// What may go out now: it refills at the rate up to [`PACING_BURST_BYTES`] and goes below
  /// zero by the datagram that took the last of it.
  budget: f64,
  maybe_refilled_at: Option<Instant>,
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
        pacer: Pacer::default(),
        departures: Vec::new(),
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
    let is_paced = is_video_frame(datagram);
    if state.queue.is_empty()
      && self.shared.is_keeping_up(&mut state, now)
      && (!is_paced || state.pacer.allows(datagram.len(), Duration::ZERO, now))
    {
      // Nothing waits, QUIC keeps up and the pacer lets it: as before, without the task.
      if is_paced {
        state.pacer.spend(datagram.len());
      }
      self.shared.send(&mut state, datagram);
      return;
    }
    state.queue.push(datagram.to_vec(), now);
    drop(state);
    self.shared.wake.notify_one();
  }

  /// Paces the video at [`PACING_FACTOR`] times `estimate_bps`, the uplink's estimate; 0 (the
  /// video held for its voice) keeps the pace as it was.
  pub fn pace_video_for(&self, estimate_bps: u32) {
    if estimate_bps > 0 {
      lock(&self.shared.state)
        .pacer
        .set_rate_bps(f64::from(estimate_bps) * PACING_FACTOR);
    }
  }

  /// The paced datagrams that left the queue since the last call, with when they left (see the
  /// module docs).
  pub fn take_departures(&self) -> Vec<(TransportSeq, Instant)> {
    std::mem::take(&mut lock(&self.shared.state).departures)
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

impl Pacer {
  /// Paces at `bps`.
  pub fn set_rate_bps(&mut self, bps: f64) {
    self.maybe_rate = Some(bps / 8.0);
  }

  /// Whether video may go out at `now` while `waiting` bytes wait for it (the datagram at hand
  /// included), the oldest of them for `waited`: at the rate, or as fast as sends them all
  /// within [`MAX_PACING_DELAY`] of the oldest one's start.
  pub fn allows(&mut self, waiting: usize, waited: Duration, now: Instant) -> bool {
    let Some(rate) = self.maybe_rate else {
      return true;
    };
    let left = MAX_PACING_DELAY.saturating_sub(waited).max(MIN_PACING_LEFT);
    let rate = rate.max(waiting as f64 / left.as_secs_f64());
    let refill = match self.maybe_refilled_at {
      Some(at) => rate * now.saturating_duration_since(at).as_secs_f64(),
      None => PACING_BURST_BYTES,
    };
    self.budget = (self.budget + refill).min(PACING_BURST_BYTES);
    self.maybe_refilled_at = Some(now);
    self.budget > 0.0
  }

  /// Whether it paces at all (a rate was set).
  pub fn is_pacing(&self) -> bool {
    self.maybe_rate.is_some()
  }

  /// `bytes` of video went out.
  pub fn spend(&mut self, bytes: usize) {
    if self.maybe_rate.is_some() {
      self.budget -= bytes as f64;
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
        let (waiting, waited) = (state.queue.queued_bytes(), state.queue.oldest_wait(now));
        if state.queue.is_video_next() && !state.pacer.allows(waiting, waited, now) {
          break;
        }
        if let Some(datagram) = state.queue.pop(now) {
          if is_video_frame(&datagram) && state.pacer.is_pacing() {
            state.pacer.spend(datagram.len());
            if let Ok(header) = DatagramHeader::parse(&datagram)
              && let Some(seq) = header.maybe_transport_seq
              && state.departures.len() < MAX_DEPARTURES
            {
              state.departures.push((seq, now));
            }
          }
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

  /// #2218: a slide of 150 kB at once leaves at the pace, 5.8 Mbit/s for an estimate of 2.9,
  /// not as fast as QUIC takes it.
  #[test]
  fn a_burst_leaves_at_the_pace() {
    let start = Instant::now();
    let mut pacer = Pacer::default();
    pacer.set_rate_bps(2_900_000.0 * PACING_FACTOR);
    let mut waiting: usize = 150_000;
    let mut sent_at = Vec::new();
    for tick in 0..400u64 {
      let now = start + ms(tick);
      while waiting > 0 && pacer.allows(waiting, ms(tick), now) {
        pacer.spend(1_200);
        waiting = waiting.saturating_sub(1_200);
        sent_at.push(tick);
      }
    }
    let took = ms(*sent_at.last().unwrap());
    // 150 kB at 5.8 Mbit/s: 207 ms; within the longest delay, a few milliseconds sooner.
    assert!(
      (ms(170)..=MAX_PACING_DELAY + ms(5)).contains(&took),
      "took {took:?}"
    );
    let first_tick = sent_at.iter().filter(|tick| **tick == 0).count();
    assert!(first_tick <= 5, "{first_tick} datagrams at once");
  }

  /// A low estimate (a share's start) doesn't hold a keyframe back for seconds: the pacer speeds
  /// up for what waits.
  #[test]
  fn a_backlog_goes_out_within_the_longest_delay() {
    let start = Instant::now();
    let mut pacer = Pacer::default();
    pacer.set_rate_bps(300_000.0);
    let mut waiting: usize = 180_000;
    let mut last = 0;
    for tick in 0..2_000u64 {
      let now = start + ms(tick);
      while waiting > 0 && pacer.allows(waiting, ms(tick), now) {
        pacer.spend(1_200);
        waiting = waiting.saturating_sub(1_200);
        last = tick;
      }
    }
    assert_eq!(waiting, 0);
    assert!(ms(last) <= MAX_PACING_DELAY + ms(20), "took {last} ms");
  }

  #[test]
  fn an_unpaced_sender_lets_everything_out() {
    let mut pacer = Pacer::default();
    let now = Instant::now();
    for _ in 0..1_000 {
      assert!(pacer.allows(1_000_000, Duration::ZERO, now));
      pacer.spend(1_200);
    }
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
