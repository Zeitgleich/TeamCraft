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
use web_time::Instant;

use crate::call_resilience::RoundTripTimer;
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
  pub fn new(config: MediaQueueConfig) -> Self {
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
      queue.push(datagram.to_vec(), Instant::now());
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
    lock(&self.shared.round_trip).on_answer(u64::from_be_bytes(number), Instant::now());
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
    lock(&self.shared.round_trip).on_ping(number, Instant::now());
    Message::Ping(Bytes::copy_from_slice(&number.to_be_bytes()))
  }

  fn finish(&self) {
    self.shared.is_finished.store(true, Ordering::Release);
    self.shared.finished.notify_waiters();
  }

  fn take_close(&self) -> Option<CloseRequest> {
    lock(&self.shared.maybe_close).take()
  }

  fn pop_media(&self) -> Option<Vec<u8>> {
    lock(&self.shared.queue).pop(Instant::now())
  }

  fn has_media(&self) -> bool {
    !lock(&self.shared.queue).is_empty()
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
  let start = tokio::time::Instant::now() + config.ping_interval;
  let mut pings = tokio::time::interval_at(start, config.ping_interval);
  pings.set_missed_tick_behavior(MissedTickBehavior::Delay);
  let mut has_control = true;
  loop {
    if let Some(close) = sender.take_close() {
      send_close(sink, &close).await;
      return WriterEnd::Closed;
    }
    let has_media = sender.has_media();
    let written = tokio::select! {
      biased;
      maybe_frame = control.next_frame(), if has_control => match maybe_frame {
        Some(frame) => send(sink, binary(frame), config.write_timeout).await,
        // The session is done with signaling; a close follows, or the socket fails.
        None => {
          has_control = false;
          true
        },
      },
      _ = pings.tick() => send(sink, sender.ping(), config.write_timeout).await,
      () = std::future::ready(()), if has_media => write_media(sink, sender, config.write_timeout).await,
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
    Some(datagram) => within(write_timeout, sink.feed(binary(datagram_frame(&datagram)))).await,
    // All of it went stale: what was fed before may still wait for its flush.
    None => true,
  };
  if !is_written {
    return false;
  }
  if sender.has_media() {
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
