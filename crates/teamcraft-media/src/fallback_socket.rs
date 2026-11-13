//! The sending side of a call's WebSocket fallback, for the SFU and the native clients alike
//! (ARCHITECTURE.md §6.6): one task owns the socket's write half and takes what to send from a
//! [`FallbackSender`] (media, without waiting, through the bounded [`MediaQueue`]) and from a
//! [`ControlFrames`] source (signaling, which is never dropped).
//!
//! A write that doesn't finish in [`WriterConfig::write_timeout`] ends the session: a peer that
//! stopped reading must not keep the task, and the session's memory, for as long as TCP retries.
//!
//! The writer's pings carry a number, which the peer's WebSocket sends back in its pong: the reader
//! hands that to [`FallbackSender::on_pong`], and [`FallbackSender::round_trip`] is the path's round
//! trip, which QUIC would measure by itself (#1227).
//!
//! A paced sender ([`FallbackSender::paced`], the SFU's) also measures from those answers what the
//! path drains, and writes video no faster ([`crate::fallback_pacer`], #1810): what TCP took can't
//! be dropped any more, so the rest waits in the queue, where it can.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::{Sink, SinkExt};
use teamcraft_proto::FallbackFrame;
use tokio::sync::Notify;
use tokio::time::MissedTickBehavior;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Bytes, Error as WebSocketError, Message};
use tracing::debug;
// The std clock, as the queue and the pacer take it, but read through tokio's: tests pause it.
use web_time::Instant;

use crate::call_resilience::RoundTripTimer;
use crate::datagram::StreamId;
use crate::fallback_pacer::{DrainPacer, PacingConfig};
use crate::fallback_queue::{MediaQueue, MediaQueueConfig};

/// Gives a peer that hasn't finished reading the close some time; the socket closes after it anyway.
const CLOSE_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// The handle to a session's writer task. Cloned freely: every clone feeds the same queue.
#[derive(Clone)]
pub struct FallbackSender {
  shared: Arc<SenderShared>,
}

struct SenderShared {
  queue: Mutex<MediaQueue>,
  /// Wakes the writer for media or a close.
  wake: Notify,
  maybe_close: Mutex<Option<CloseRequest>>,
  is_finished: AtomicBool,
  finished: Notify,
  /// Datagrams the queue dropped, kept here so the metrics needn't take its lock.
  dropped_packets: AtomicU64,
  /// The number of the next ping.
  next_ping: AtomicU64,
  round_trip: Mutex<RoundTripTimer>,
  /// Locked after `queue` where both are needed.
  maybe_pacer: Mutex<Option<DrainPacer>>,
}

/// What the writer can do with the media queue now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NextMedia {
  Nothing,
  Ready,
  /// Only video waits, for the pacer.
  After(Duration),
}

/// A close to send when the writer gets to it.
#[derive(Debug, Clone)]
struct CloseRequest {
  code: u32,
  reason: Vec<u8>,
}

/// Where a writer gets its signaling frames from, already encoded.
pub trait ControlFrames: Send {
  /// The next frame, or `None` when there won't be any more. Must be cancel-safe.
  fn next_frame(&mut self) -> impl Future<Output = Option<Vec<u8>>> + Send;
}

/// How a writer behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriterConfig {
  /// WebSocket pings while the call runs: they keep proxies from closing a quiet socket, and their
  /// answers prove the path alive to both sides.
  pub ping_interval: Duration,
  /// A single write may take this long.
  pub write_timeout: Duration,
}

/// Why a writer stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriterEnd {
  /// It sent a close on purpose.
  Closed,
  /// The socket failed, or the peer stopped reading.
  Broken,
}

impl FallbackSender {
  /// A sender that writes media as fast as the socket takes it.
  pub fn new(config: MediaQueueConfig) -> Self {
    Self::with_pacer(config, None)
  }

  /// A sender that writes video no faster than the path drains, measured with a ping every
  /// [`PacingConfig::probe_interval`].
  pub fn paced(config: MediaQueueConfig, pacing: PacingConfig) -> Self {
    Self::with_pacer(config, Some(DrainPacer::new(pacing)))
  }

  fn with_pacer(config: MediaQueueConfig, maybe_pacer: Option<DrainPacer>) -> Self {
    Self {
      shared: Arc::new(SenderShared {
        queue: Mutex::new(MediaQueue::new(config)),
        wake: Notify::new(),
        maybe_close: Mutex::new(None),
        is_finished: AtomicBool::new(false),
        finished: Notify::new(),
        dropped_packets: AtomicU64::new(0),
        next_ping: AtomicU64::new(0),
        round_trip: Mutex::new(RoundTripTimer::default()),
        maybe_pacer: Mutex::new(maybe_pacer),
      }),
    }
  }

  /// Queues one datagram. Unreliable by design: what doesn't fit, or waits too long, is dropped.
  pub fn send_datagram(&self, datagram: &[u8]) {
    if self.shared.is_finished.load(Ordering::Relaxed) {
      return;
    }
    let dropped = {
      let mut queue = lock(&self.shared.queue);
      queue.push(datagram.to_vec(), now());
      queue.dropped_packets()
    };
    self.shared.dropped_packets.store(dropped, Ordering::Relaxed);
    self.shared.wake.notify_one();
  }

  /// Asks the writer to send a close and stop. The first close counts.
  pub fn close(&self, code: u32, reason: &[u8]) {
    let mut maybe_close = lock(&self.shared.maybe_close);
    if maybe_close.is_none() {
      *maybe_close = Some(CloseRequest {
        code,
        reason: reason.to_vec(),
      });
    }
    drop(maybe_close);
    self.shared.wake.notify_one();
  }

  /// Resolves when the writer stopped.
  pub async fn wait_finished(&self) {
    let finished = self.shared.finished.notified();
    if !self.shared.is_finished.load(Ordering::Acquire) {
      finished.await;
    }
  }

  /// Datagrams dropped so far.
  pub fn dropped_packets(&self) -> u64 {
    self.shared.dropped_packets.load(Ordering::Relaxed)
  }

  /// The peer answered a ping with this payload (a WebSocket pong).
  pub fn on_pong(&self, payload: &[u8]) {
    let Ok(number) = <[u8; 8]>::try_from(payload) else {
      return;
    };
    let number = u64::from_be_bytes(number);
    let now = now();
    lock(&self.shared.round_trip).on_answer(number, now);
    if let Some(pacer) = lock(&self.shared.maybe_pacer).as_mut() {
      pacer.on_answer(number, now);
    }
  }

  /// The rate video is paced to, in bits per second: what the path drains, about. `None` unpaced.
  /// The streams whose video the queue had to drop since the last call: their receiver needs a
  /// keyframe of each to show it again (#1845).
  pub fn take_keyframe_needs(&self) -> Vec<StreamId> {
    lock(&self.shared.queue).take_keyframe_needs()
  }

  pub fn pacing_rate_bps(&self) -> Option<u32> {
    lock(&self.shared.maybe_pacer).as_ref().map(DrainPacer::rate_bps)
  }

  /// The round trip of the socket, from the answers to its pings; `None` before the first.
  pub fn round_trip(&self) -> Option<Duration> {
    lock(&self.shared.round_trip).smoothed()
  }

  /// [`Self::ping`], for the reader's tests.
  #[cfg(test)]
  pub(crate) fn ping_for_test(&self) -> Message {
    self.ping()
  }

  /// The next ping, numbered, noted as sent now.
  fn ping(&self) -> Message {
    let number = self.shared.next_ping.fetch_add(1, Ordering::Relaxed);
    let now = now();
    lock(&self.shared.round_trip).on_ping(number, now);
    let payload = number.to_be_bytes();
    if let Some(pacer) = lock(&self.shared.maybe_pacer).as_mut() {
      pacer.on_probe_sent(number, now);
      pacer.on_written(payload.len(), now);
    }
    Message::Ping(Bytes::copy_from_slice(&payload))
  }

  /// How often the writer pings: as configured, or more often to measure the path for the pacer.
  fn ping_interval(&self, configured: Duration) -> Duration {
    lock(&self.shared.maybe_pacer)
      .as_ref()
      .map_or(configured, |pacer| pacer.probe_interval().min(configured))
  }

  /// Something other than media was written: video waits for it too.
  fn note_written(&self, bytes: usize) {
    if let Some(pacer) = lock(&self.shared.maybe_pacer).as_mut() {
      pacer.on_written(bytes, now());
    }
  }

  fn finish(&self) {
    self.shared.is_finished.store(true, Ordering::Release);
    self.shared.finished.notify_waiters();
  }

  fn take_close(&self) -> Option<CloseRequest> {
    lock(&self.shared.maybe_close).take()
  }

  /// The next datagram, encoded as a frame, if one may go now.
  fn pop_media(&self) -> Option<Vec<u8>> {
    let now = now();
    let mut queue = lock(&self.shared.queue);
    let mut maybe_pacer = lock(&self.shared.maybe_pacer);
    let may_send_video = maybe_pacer
      .as_mut()
      .is_none_or(|pacer| pacer.wait_for(now).is_zero());
    let frame = datagram_frame(&queue.pop_paced(may_send_video, now)?);
    if let Some(pacer) = maybe_pacer.as_mut() {
      pacer.on_written(frame.len(), now);
    }
    Some(frame)
  }

  fn next_media(&self) -> NextMedia {
    let queue = lock(&self.shared.queue);
    if queue.is_empty() {
      return NextMedia::Nothing;
    }
    if queue.has_audio() {
      return NextMedia::Ready;
    }
    match lock(&self.shared.maybe_pacer).as_mut() {
      Some(pacer) => match pacer.wait_for(now()) {
        Duration::ZERO => NextMedia::Ready,
        wait => NextMedia::After(wait),
      },
      None => NextMedia::Ready,
    }
  }
}

/// Writes until a close is asked for or the socket fails. Signaling goes before media; media is
/// written in batches and flushed when none is left to add.
pub async fn run_writer<S, C>(
  mut sink: S,
  mut control: C,
  sender: FallbackSender,
  config: WriterConfig,
) -> WriterEnd
where
  S: Sink<Message, Error = WebSocketError> + Unpin + Send,
  C: ControlFrames,
{
  let end = write_until_end(&mut sink, &mut control, &sender, config).await;
  sender.finish();
  end
}

async fn write_until_end<S, C>(
  sink: &mut S,
  control: &mut C,
  sender: &FallbackSender,
  config: WriterConfig,
) -> WriterEnd
where
  S: Sink<Message, Error = WebSocketError> + Unpin + Send,
  C: ControlFrames,
{
  let ping_interval = sender.ping_interval(config.ping_interval);
  let start = tokio::time::Instant::now() + ping_interval;
  let mut pings = tokio::time::interval_at(start, ping_interval);
  pings.set_missed_tick_behavior(MissedTickBehavior::Delay);
  let mut has_control = true;
  loop {
    if let Some(close) = sender.take_close() {
      send_close(sink, &close).await;
      return WriterEnd::Closed;
    }
    let next_media = sender.next_media();
    let pacing_wait = match next_media {
      NextMedia::After(wait) => wait,
      NextMedia::Nothing | NextMedia::Ready => Duration::ZERO,
    };
    let written = tokio::select! {
      biased;
      maybe_frame = control.next_frame(), if has_control => match maybe_frame {
        Some(frame) => {
          sender.note_written(frame.len());
          send(sink, binary(frame), config.write_timeout).await
        },
        // The session is done with signaling; a close follows, or the socket fails.
        None => {
          has_control = false;
          true
        },
      },
      _ = pings.tick() => send(sink, sender.ping(), config.write_timeout).await,
      () = std::future::ready(()), if next_media == NextMedia::Ready => {
        write_media(sink, sender, config.write_timeout).await
      },
      // Video waits for the pacer; what is queued meanwhile wakes the writer below.
      () = tokio::time::sleep(pacing_wait), if !pacing_wait.is_zero() => true,
      () = sender.shared.wake.notified() => true,
    };
    if !written {
      return WriterEnd::Broken;
    }
  }
}

/// One datagram, and a flush if it was the last one for now. `false` when the socket failed.
async fn write_media<S>(sink: &mut S, sender: &FallbackSender, write_timeout: Duration) -> bool
where
  S: Sink<Message, Error = WebSocketError> + Unpin + Send,
{
  let is_written = match sender.pop_media() {
    Some(frame) => within(write_timeout, sink.feed(binary(frame))).await,
    // All of it went stale: what was fed before may still wait for its flush.
    None => true,
  };
  if !is_written {
    return false;
  }
  if sender.next_media() == NextMedia::Ready {
    return true;
  }
  within(write_timeout, sink.flush()).await
}

async fn send<S>(sink: &mut S, message: Message, write_timeout: Duration) -> bool
where
  S: Sink<Message, Error = WebSocketError> + Unpin + Send,
{
  within(write_timeout, sink.send(message)).await
}

/// Whether `write` finished in time. [`tokio::time::timeout`] polls the write before its deadline,
/// so one that a late wake-up finds done would count as in time (a busy machine, or a peer's TCP
/// that lets a few bytes through with each window probe): it was a stall all the same (#1556).
async fn within(write_timeout: Duration, write: impl Future<Output = Result<(), WebSocketError>>) -> bool {
  let started_at = tokio::time::Instant::now();
  match tokio::time::timeout(write_timeout, write).await {
    Ok(Ok(())) if started_at.elapsed() >= write_timeout => {
      debug!("the peer took as long as the write timeout to take a write");
      false
    },
    Ok(Ok(())) => true,
    Ok(Err(error)) => {
      debug!(%error, "call socket write failed");
      false
    },
    Err(_) => {
      debug!("the peer stopped reading the call socket");
      false
    },
  }
}

/// Best effort: the session ends either way.
async fn send_close<S>(sink: &mut S, close: &CloseRequest)
where
  S: Sink<Message, Error = WebSocketError> + Unpin + Send,
{
  let frame = FallbackFrame::Close {
    code: close.code,
    reason: &close.reason,
  }
  .encode();
  let closing = async {
    sink.send(binary(frame)).await?;
    sink
      .send(Message::Close(Some(CloseFrame {
        code: CloseCode::Normal,
        reason: "".into(),
      })))
      .await
  };
  if !matches!(
    tokio::time::timeout(CLOSE_WRITE_TIMEOUT, closing).await,
    Ok(Ok(()))
  ) {
    debug!("the close did not reach the call socket");
  }
}

fn binary(frame: Vec<u8>) -> Message {
  Message::Binary(Bytes::from(frame))
}

fn datagram_frame(datagram: &[u8]) -> Vec<u8> {
  FallbackFrame::Datagram(datagram).encode()
}

/// Now, on tokio's clock: the real one, or the paused one of a test.
fn now() -> Instant {
  tokio::time::Instant::now().into_std()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  // A panicked writer leaves a queue that is still consistent enough to drop.
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
  use std::collections::VecDeque;
  use std::pin::Pin;
  use std::sync::Mutex as StdMutex;
  use std::task::{Context, Poll};

  use teamcraft_proto::FallbackFrame;
  use tokio::sync::mpsc;

  use super::*;

  const CONFIG: WriterConfig = WriterConfig {
    ping_interval: Duration::from_secs(3600),
    write_timeout: Duration::from_secs(1),
  };

  mod writing {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn signaling_goes_before_media_and_a_close_comes_last() {
      let sent = Arc::new(StdMutex::new(Vec::new()));
      let (control_tx, control) = control();
      let sender = FallbackSender::new(MediaQueueConfig::default());
      sender.send_datagram(b"media");
      control_tx.send(b"signal".to_vec()).await.unwrap();
      let writer = tokio::spawn(run_writer(open_sink(&sent), control, sender.clone(), CONFIG));
      tokio::task::yield_now().await;
      sender.close(0x4805, b"going away");
      assert_eq!(writer.await.unwrap(), WriterEnd::Closed);
      let sent = sent.lock().unwrap();
      assert_eq!(sent[0], binary(b"signal".to_vec()));
      assert_eq!(sent[1], binary(FallbackFrame::Datagram(b"media").encode()));
      assert_eq!(
        sent[2],
        binary(
          FallbackFrame::Close {
            code: 0x4805,
            reason: b"going away"
          }
          .encode()
        )
      );
      assert!(matches!(sent[3], Message::Close(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_quiet_session_sends_pings_and_nothing_else() {
      let sent = Arc::new(StdMutex::new(Vec::new()));
      let (_control_tx, control) = control();
      let sender = FallbackSender::new(MediaQueueConfig::default());
      let config = WriterConfig {
        ping_interval: Duration::from_secs(1),
        ..CONFIG
      };
      let writer = tokio::spawn(run_writer(open_sink(&sent), control, sender.clone(), config));
      tokio::time::sleep(Duration::from_millis(3500)).await;
      sender.close(0, b"");
      writer.await.unwrap();
      let sent = sent.lock().unwrap();
      assert_eq!(sent.iter().filter(|m| matches!(m, Message::Ping(_))).count(), 3);
      // Then the close frame and the WebSocket's close.
      assert_eq!(sent.len(), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn a_peer_that_stopped_reading_ends_the_writer() {
      let (_control_tx, control) = control();
      let sender = FallbackSender::new(MediaQueueConfig::default());
      sender.send_datagram(b"media");
      let end = run_writer(StalledSink, control, sender.clone(), CONFIG).await;
      assert_eq!(end, WriterEnd::Broken);
      sender.wait_finished().await;
    }

    /// #1556: found finished only when the deadline passed, a write was still too slow.
    #[tokio::test(start_paused = true)]
    async fn a_write_that_took_the_whole_timeout_ends_the_writer() {
      let (_control_tx, control) = control();
      let sender = FallbackSender::new(MediaQueueConfig::default());
      sender.send_datagram(b"media");
      let sink = SlowSink {
        delay: CONFIG.write_timeout,
        maybe_sleep: None,
        written: Arc::new(StdMutex::new(VecDeque::new())),
      };
      assert_eq!(
        run_writer(sink, control, sender.clone(), CONFIG).await,
        WriterEnd::Broken
      );
    }

    #[tokio::test(start_paused = true)]
    async fn media_for_a_finished_writer_is_not_queued() {
      let (_control_tx, control) = control();
      let sender = FallbackSender::new(MediaQueueConfig {
        max_bytes: 1,
        max_age: Duration::from_secs(1),
      });
      sender.send_datagram(b"x");
      run_writer(StalledSink, control, sender.clone(), CONFIG).await;
      let before = sender.dropped_packets();
      sender.send_datagram(b"too big for the queue");
      assert_eq!(sender.dropped_packets(), before);
    }
  }

  mod slow_peer {
    use super::*;

    /// A peer that takes `delay` for each message while a sender offers 2 MB/s for 5 s: the queue
    /// stays within its bound whatever the socket does, and what is written is recent.
    #[tokio::test(start_paused = true)]
    async fn memory_stays_bounded_and_what_goes_out_is_recent() {
      let config = MediaQueueConfig {
        max_bytes: 64 * 1024,
        max_age: Duration::from_millis(300),
      };
      let sender = FallbackSender::new(config);
      let (_control_tx, control) = control();
      let written = Arc::new(StdMutex::new(VecDeque::new()));
      let sink = SlowSink {
        delay: Duration::from_millis(10),
        maybe_sleep: None,
        written: Arc::clone(&written),
      };
      let writer = tokio::spawn(run_writer(sink, control, sender.clone(), CONFIG));
      for tick in 0..2500u32 {
        sender.send_datagram(&marked(tick));
        let queued = lock(&sender.shared.queue).queued_bytes();
        assert!(queued <= config.max_bytes, "{queued}");
        tokio::time::sleep(Duration::from_millis(2)).await;
      }
      sender.close(0, b"");
      writer.await.unwrap();
      assert!(sender.dropped_packets() > 0);
      let written = written.lock().unwrap();
      // Each one was written within the age bound (and the write it waited for) of being queued.
      let late = written
        .iter()
        .filter(|(tick, at_ms)| at_ms - u64::from(*tick) * 2 > 320)
        .count();
      assert_eq!(late, 0);
    }

    fn marked(tick: u32) -> Vec<u8> {
      let mut bytes = vec![0u8; 1000];
      bytes[..4].copy_from_slice(&tick.to_be_bytes());
      bytes
    }
  }

  /// #1810: a viewer on the fallback behind a link that swings between 1 and 30 Mbit/s, with dips
  /// of a few seconds, for two minutes. Between the SFU and the link sit buffers of megabytes (the
  /// kernel's and a reverse proxy's), which TCP fills as fast as the writer gives it media. The
  /// paced writer keeps them nearly empty: the session never stalls, the voice keeps coming with
  /// little delay, and the video (chosen from the pacing rate, as the SFU's layer allocation does)
  /// steps down in a dip and back up after it.
  mod fluctuating_link {
    use std::task::Waker;

    use crate::fallback_pacer::PacingConfig;
    use crate::rtp::PayloadType;

    use super::*;

    const SEED: u64 = 1810;
    const RUN: Duration = Duration::from_secs(120);
    /// What the kernel's autotuned send buffer and a proxy's hold between the writer and the link.
    const PATH_BUFFER_BYTES: usize = 4 * 1024 * 1024;
    const ONE_WAY_DELAY: Duration = Duration::from_millis(20);
    const LINK_TICK: Duration = Duration::from_millis(5);
    const RATE_SLOT: Duration = Duration::from_millis(200);
    const AUDIO_INTERVAL: Duration = Duration::from_millis(20);
    const AUDIO_BYTES: usize = 120;
    const DATAGRAM_BYTES: usize = 1200;
    /// The video layers, as bit rates; the lowest is never paused.
    const LAYERS_BPS: [u32; 4] = [250_000, 600_000, 1_200_000, 2_500_000];
    const AUDIO_RESERVE_BPS: u32 = 64_000;
    const WARM_UP: Duration = Duration::from_secs(3);

    #[tokio::test(start_paused = true)]
    async fn a_fluctuating_link_costs_quality_never_the_session() {
      let rates = link_rates();
      let run = run_call(&rates, true).await;
      assert!(!run.writer_ended, "the session ended");

      let audio = run.audio_after(WARM_UP);
      let expected = (RUN - WARM_UP).as_millis() / AUDIO_INTERVAL.as_millis();
      // A little is lost to the queue's age bound in the worst moment of a sudden drop.
      assert!(
        audio.len() as u128 * 100 >= expected * 97,
        "{} of {expected}",
        audio.len()
      );
      let max_gap = audio
        .windows(2)
        .map(|pair| pair[1].delivered_ms - pair[0].delivered_ms)
        .max()
        .unwrap();
      assert!(max_gap <= 500, "the voice stopped for {max_gap} ms");
      let mut latencies: Vec<u64> = audio.iter().map(Delivery::latency_ms).collect();
      latencies.sort_unstable();
      let p95 = latencies[latencies.len() * 95 / 100];
      let max = *latencies.last().unwrap();
      assert!(p95 <= 300, "p95 voice delay {p95} ms");
      assert!(max <= 1500, "voice delay up to {max} ms");

      // Down in a dip (the layer is chosen at the start of a second, from what the pacer saw), up
      // again after it, and not stuck at the lowest layer the rest of the time either.
      let dips: Vec<usize> = (1..run.layers.len())
        .filter(|&second| (second - 1..=second).all(|s| second_rate(&rates, s) < 1_500_000))
        .collect();
      assert!(!dips.is_empty(), "the seed gave no dip");
      for &second in &dips {
        assert!(
          run.layers[second] <= 1,
          "layer {} in the dip at {second} s",
          run.layers[second]
        );
      }
      let good: Vec<usize> = (5..run.layers.len())
        .filter(|&second| (second - 5..=second).all(|s| second_rate(&rates, s) > 5_000_000))
        .collect();
      assert!(!good.is_empty(), "the seed gave no good stretch");
      let up = good.iter().filter(|&&second| run.layers[second] >= 2).count();
      assert!(
        up * 10 >= good.len() * 8,
        "{up} of {} good seconds sharp",
        good.len()
      );
      assert!(
        good.iter().any(|&second| second > dips[0]),
        "no recovery after a dip"
      );
      let lowest = run.layers.iter().filter(|&&layer| layer == 0).count();
      assert!(lowest * 10 < run.layers.len(), "{lowest} s at the lowest layer");
    }

    /// The same link without pacing: the path's buffers fill, and the voice arrives seconds late.
    /// What the test above asserts is the pacer's doing.
    #[tokio::test(start_paused = true)]
    async fn without_pacing_the_path_backs_up() {
      let run = run_call(&link_rates(), false).await;
      let max = run
        .audio_after(WARM_UP)
        .iter()
        .map(Delivery::latency_ms)
        .max()
        .unwrap();
      assert!(max > 2000, "voice delay up to {max} ms");
    }

    struct Run {
      writer_ended: bool,
      deliveries: Vec<Delivery>,
      /// The layer chosen in each second.
      layers: Vec<usize>,
    }

    #[derive(Debug, Clone, Copy)]
    struct Delivery {
      is_audio: bool,
      sent_ms: u64,
      delivered_ms: u64,
    }

    /// The path between the writer and the viewer: buffers, then the link.
    struct Path {
      buffered: VecDeque<Message>,
      buffered_bytes: usize,
      maybe_waker: Option<Waker>,
    }

    struct PathSink(Arc<StdMutex<Path>>);

    async fn run_call(rates: &[u32], is_paced: bool) -> Run {
      let start = tokio::time::Instant::now();
      let queue = MediaQueueConfig::default();
      let sender = if is_paced {
        FallbackSender::paced(queue, PacingConfig::default())
      } else {
        FallbackSender::new(queue)
      };
      let path = Arc::new(StdMutex::new(Path {
        buffered: VecDeque::new(),
        buffered_bytes: 0,
        maybe_waker: None,
      }));
      let (_control_tx, control) = control();
      let config = WriterConfig {
        ping_interval: Duration::from_secs(1),
        write_timeout: Duration::from_secs(5),
      };
      let writer = tokio::spawn(run_writer(
        PathSink(Arc::clone(&path)),
        control,
        sender.clone(),
        config,
      ));
      let link = tokio::spawn(run_link(Arc::clone(&path), sender.clone(), rates.to_vec(), start));
      let layers = run_source(&sender, start).await;
      let writer_ended = writer.is_finished();
      sender.close(0, b"");
      let _ = writer.await;
      let deliveries = link.await.unwrap();
      Run {
        writer_ended,
        deliveries,
        layers,
      }
    }

    /// What the SFU forwards: a voice packet every 20 ms, 25 video frames a second at the layer the
    /// pacing rate allows (once a second, as the layer allocation's holds would). Returns the layers.
    async fn run_source(sender: &FallbackSender, start: tokio::time::Instant) -> Vec<usize> {
      let mut ticks = tokio::time::interval(AUDIO_INTERVAL);
      let mut layer = 0;
      let mut layers = Vec::new();
      let ticks_per_second = 1000 / AUDIO_INTERVAL.as_millis();
      for tick in 0..RUN.as_millis() / AUDIO_INTERVAL.as_millis() {
        ticks.tick().await;
        let sent_ms = elapsed_ms(start);
        if tick % ticks_per_second == 0 {
          let rate_bps = sender.pacing_rate_bps().unwrap_or(u32::MAX);
          layer = LAYERS_BPS
            .iter()
            .rposition(|&bps| bps / 4 * 5 + AUDIO_RESERVE_BPS <= rate_bps)
            .unwrap_or(0);
          layers.push(layer);
        }
        sender.send_datagram(&datagram(PayloadType::OPUS.value(), AUDIO_BYTES, sent_ms));
        if tick % 2 == 0 {
          let frame_bytes = (LAYERS_BPS[layer] / 8 / 25) as usize;
          for _ in 0..frame_bytes.div_ceil(DATAGRAM_BYTES) {
            sender.send_datagram(&datagram(100, DATAGRAM_BYTES, sent_ms));
          }
        }
      }
      layers
    }

    /// Drains the path at the link's rate until the writer is done; answers pings a round trip
    /// later, as the viewer's WebSocket does once it read them.
    async fn run_link(
      path: Arc<StdMutex<Path>>,
      sender: FallbackSender,
      rates: Vec<u32>,
      start: tokio::time::Instant,
    ) -> Vec<Delivery> {
      let mut deliveries = Vec::new();
      let mut pongs: VecDeque<(u64, Bytes)> = VecDeque::new();
      let mut budget_bytes = 0.0;
      let mut ticks = tokio::time::interval(LINK_TICK);
      let one_way_ms = u64::try_from(ONE_WAY_DELAY.as_millis()).unwrap();
      while !sender.shared.is_finished.load(Ordering::Acquire) {
        ticks.tick().await;
        let now_ms = elapsed_ms(start);
        while pongs.front().is_some_and(|(due_ms, _)| *due_ms <= now_ms) {
          let (_, payload) = pongs.pop_front().unwrap();
          sender.on_pong(&payload);
        }
        let slot = usize::try_from(now_ms / u64::try_from(RATE_SLOT.as_millis()).unwrap()).unwrap();
        let rate_bps = rates[slot.min(rates.len() - 1)];
        budget_bytes += f64::from(rate_bps) / 8.0 * LINK_TICK.as_secs_f64();
        let mut path = path.lock().unwrap();
        while let Some(message) = path.buffered.front() {
          let length = message.len() + FRAME_OVERHEAD;
          if budget_bytes < length as f64 {
            break;
          }
          budget_bytes -= length as f64;
          path.buffered_bytes -= length;
          let message = path.buffered.pop_front().unwrap();
          let arrives_ms = now_ms + one_way_ms;
          match message {
            Message::Ping(payload) => pongs.push_back((arrives_ms + one_way_ms, payload)),
            Message::Binary(bytes) => {
              if let Ok(FallbackFrame::Datagram(body)) = FallbackFrame::decode(&bytes) {
                deliveries.push(Delivery {
                  is_audio: body[6] & 0x7F == PayloadType::OPUS.value(),
                  sent_ms: u64::from_be_bytes(body[body.len() - 8..].try_into().unwrap()),
                  delivered_ms: arrives_ms,
                });
              }
            },
            _ => {},
          }
        }
        if path.buffered.is_empty() {
          // An idle link saves nothing up.
          budget_bytes = 0.0;
        }
        if path.buffered_bytes < PATH_BUFFER_BYTES
          && let Some(waker) = path.maybe_waker.take()
        {
          waker.wake();
        }
      }
      deliveries
    }

    /// Seeded: mostly 1.5 to 9 Mbit/s, dips to 1 to 2 Mbit/s for 2 to 5 seconds, now and then a few
    /// seconds of 20 to 30, and ±15 % within a second. A rate per [`RATE_SLOT`].
    fn link_rates() -> Vec<u32> {
      let mut rng = SplitMix(SEED);
      let slots_per_second = usize::try_from(1000 / RATE_SLOT.as_millis()).unwrap();
      let mut rates = Vec::new();
      while rates.len() < usize::try_from(RUN.as_secs()).unwrap() * slots_per_second {
        let pick = rng.unit();
        let (seconds, mbps) = if pick < 0.15 {
          (2 + rng.below(4), 1.0 + rng.unit())
        } else if pick < 0.2 {
          (3 + rng.below(3), 20.0 + 10.0 * rng.unit())
        } else {
          (1 + rng.below(3), 1.5 + 7.5 * rng.unit())
        };
        for _ in 0..seconds * slots_per_second {
          let jitter = 0.85 + 0.3 * rng.unit();
          rates.push((mbps * jitter * 1_000_000.0) as u32);
        }
      }
      rates
    }

    /// The lowest rate of a second.
    fn second_rate(rates: &[u32], second: usize) -> u32 {
      let slots_per_second = usize::try_from(1000 / RATE_SLOT.as_millis()).unwrap();
      rates[second * slots_per_second..(second + 1) * slots_per_second]
        .iter()
        .copied()
        .min()
        .unwrap()
    }

    /// A 5-byte datagram header without a transport sequence number, an RTP header's start with
    /// the payload type, and the time it was sent at the end.
    fn datagram(payload_type: u8, length: usize, sent_ms: u64) -> Vec<u8> {
      let mut bytes = vec![0u8; length];
      bytes[5] = 0x80;
      bytes[6] = payload_type;
      bytes[length - 8..].copy_from_slice(&sent_ms.to_be_bytes());
      bytes
    }

    fn elapsed_ms(start: tokio::time::Instant) -> u64 {
      u64::try_from(start.elapsed().as_millis()).unwrap()
    }

    /// A WebSocket frame's header from the SFU, about.
    const FRAME_OVERHEAD: usize = 4;

    impl Run {
      fn audio_after(&self, warm_up: Duration) -> Vec<Delivery> {
        let warm_up_ms = u64::try_from(warm_up.as_millis()).unwrap();
        self
          .deliveries
          .iter()
          .filter(|delivery| delivery.is_audio && delivery.sent_ms >= warm_up_ms)
          .copied()
          .collect()
      }
    }

    impl Delivery {
      fn latency_ms(&self) -> u64 {
        self.delivered_ms - self.sent_ms
      }
    }

    impl Sink<Message> for PathSink {
      type Error = WebSocketError;
      fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), WebSocketError>> {
        let mut path = self.0.lock().unwrap();
        if path.buffered_bytes < PATH_BUFFER_BYTES {
          return Poll::Ready(Ok(()));
        }
        path.maybe_waker = Some(cx.waker().clone());
        Poll::Pending
      }
      fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), WebSocketError> {
        let mut path = self.0.lock().unwrap();
        path.buffered_bytes += item.len() + FRAME_OVERHEAD;
        path.buffered.push_back(item);
        Ok(())
      }
      fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), WebSocketError>> {
        self.poll_ready(cx)
      }
      fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), WebSocketError>> {
        Poll::Ready(Ok(()))
      }
    }

    /// A small seeded generator (SplitMix64), so the link is the same on every run.
    struct SplitMix(u64);

    impl SplitMix {
      fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
      }

      fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
      }

      fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % bound as u64).unwrap()
      }
    }
  }

  struct MpscControl(mpsc::Receiver<Vec<u8>>);

  impl ControlFrames for MpscControl {
    async fn next_frame(&mut self) -> Option<Vec<u8>> {
      self.0.recv().await
    }
  }

  fn control() -> (mpsc::Sender<Vec<u8>>, MpscControl) {
    let (tx, rx) = mpsc::channel(4);
    (tx, MpscControl(rx))
  }

  fn open_sink(sent: &Arc<StdMutex<Vec<Message>>>) -> RecordingSink {
    RecordingSink(Arc::clone(sent))
  }

  /// Takes everything at once.
  struct RecordingSink(Arc<StdMutex<Vec<Message>>>);

  impl Sink<Message> for RecordingSink {
    type Error = WebSocketError;
    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), WebSocketError>> {
      Poll::Ready(Ok(()))
    }
    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), WebSocketError> {
      self.0.lock().unwrap().push(item);
      Ok(())
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), WebSocketError>> {
      Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), WebSocketError>> {
      Poll::Ready(Ok(()))
    }
  }

  /// Never takes anything: a peer that stopped reading, its buffers full.
  struct StalledSink;

  impl Sink<Message> for StalledSink {
    type Error = WebSocketError;
    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), WebSocketError>> {
      Poll::Pending
    }
    fn start_send(self: Pin<&mut Self>, _: Message) -> Result<(), WebSocketError> {
      Ok(())
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), WebSocketError>> {
      Poll::Pending
    }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), WebSocketError>> {
      Poll::Pending
    }
  }

  /// Takes one message per `delay`, and notes which datagram when (in milliseconds of paused time).
  struct SlowSink {
    delay: Duration,
    maybe_sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    written: Arc<StdMutex<VecDeque<(u32, u64)>>>,
  }

  impl Sink<Message> for SlowSink {
    type Error = WebSocketError;
    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), WebSocketError>> {
      if let Some(sleep) = &mut self.maybe_sleep {
        if sleep.as_mut().poll(cx).is_pending() {
          return Poll::Pending;
        }
        self.maybe_sleep = None;
      }
      Poll::Ready(Ok(()))
    }
    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), WebSocketError> {
      if let Message::Binary(bytes) = &item
        && let Ok(FallbackFrame::Datagram(body)) = FallbackFrame::decode(bytes)
      {
        let tick = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
        let at_ms =
          u64::try_from(START.get_or_init(tokio::time::Instant::now).elapsed().as_millis()).unwrap();
        self.written.lock().unwrap().push_back((tick, at_ms));
      }
      let delay = self.delay;
      self.maybe_sleep = Some(Box::pin(tokio::time::sleep(delay)));
      Ok(())
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), WebSocketError>> {
      self.poll_ready(cx)
    }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), WebSocketError>> {
      Poll::Ready(Ok(()))
    }
  }

  static START: std::sync::OnceLock<tokio::time::Instant> = std::sync::OnceLock::new();
}
