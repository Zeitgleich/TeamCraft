//! The bounded queue in front of a call's WebSocket fallback (ARCHITECTURE.md §6.6).
//!
//! TCP delivers everything, in order: after a lost packet or on a link that is too narrow, whatever
//! waits behind it gets later and later. A QUIC datagram that doesn't fit is dropped by QUIC; here
//! this queue does that. It keeps the newest media within a byte bound and an age bound, so a slow
//! client costs a known amount of memory, and what finally goes out is still worth playing. Audio
//! goes first and is dropped last: people follow a call by its sound, and a voice packet is small
//! next to the video queue ahead of it.
//!
//! A dropped packet shows as a gap in the transport sequence numbers, so the bandwidth estimators
//! of `crate::bwe` see it as they see a lost QUIC datagram, and the sender or the SFU's layer
//! allocation backs off.
//!
//! Pure state: time is passed in, so all of it is tested without a network.

use std::collections::VecDeque;
use std::time::Duration;

// `std::time` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::datagram::DatagramHeader;
use crate::rtp::PayloadType;

/// A datagram's RTP header: the payload type is in its second byte, after the marker bit.
const RTP_PAYLOAD_TYPE_OFFSET: usize = 1;
const RTP_PAYLOAD_TYPE_MASK: u8 = 0x7F;

/// What a browser's WebSocket may have buffered before a video datagram is dropped; a browser can't
/// take back what it buffered, so this is little, about 100 ms at 5 Mbit/s.
const BROWSER_VIDEO_BUFFER_BYTES: u32 = 64 * 1024;
/// The same for audio, which comes in small packets and is worth more: it may wait behind some
/// video, not behind seconds of it.
const BROWSER_AUDIO_BUFFER_BYTES: u32 = 256 * 1024;

/// How much media may wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaQueueConfig {
  /// Bytes waiting, audio and video together. A bigger datagram than this is dropped at once.
  pub max_bytes: usize,
  /// What has waited this long is dropped instead of sent: it would arrive too late to play.
  pub max_age: Duration,
}

/// Media waiting to be written to the socket.
#[derive(Debug)]
pub struct MediaQueue {
  config: MediaQueueConfig,
  audio: VecDeque<Queued>,
  video: VecDeque<Queued>,
  queued_bytes: usize,
  dropped_packets: u64,
}

#[derive(Debug)]
struct Queued {
  queued_at: Instant,
  bytes: Vec<u8>,
}

impl Default for MediaQueueConfig {
  /// About a keyframe's burst, and as old as the jitter buffer of the other side can still use
  /// (`crate::jitter::JitterBufferConfig::for_fallback`).
  fn default() -> Self {
    Self {
      max_bytes: 1024 * 1024,
      max_age: Duration::from_millis(300),
    }
  }
}

impl MediaQueue {
  pub fn new(config: MediaQueueConfig) -> Self {
    Self {
      config,
      audio: VecDeque::new(),
      video: VecDeque::new(),
      queued_bytes: 0,
      dropped_packets: 0,
    }
  }

  /// Queues one datagram, dropping what doesn't fit: what is stale first, then the oldest video,
  /// then the oldest audio.
  pub fn push(&mut self, bytes: Vec<u8>, now: Instant) {
    if bytes.len() > self.config.max_bytes {
      self.dropped_packets += 1;
      return;
    }
    self.drop_stale(now);
    self.queued_bytes += bytes.len();
    let queue = if is_audio(&bytes) {
      &mut self.audio
    } else {
      &mut self.video
    };
    queue.push_back(Queued {
      queued_at: now,
      bytes,
    });
    while self.queued_bytes > self.config.max_bytes {
      let Some(dropped) = self.video.pop_front().or_else(|| self.audio.pop_front()) else {
        return;
      };
      self.forget(&dropped);
    }
  }

  /// The next datagram to write: audio first, each kind oldest first, nothing stale.
  pub fn pop(&mut self, now: Instant) -> Option<Vec<u8>> {
    self.drop_stale(now);
    let queued = self.audio.pop_front().or_else(|| self.video.pop_front())?;
    self.queued_bytes -= queued.bytes.len();
    Some(queued.bytes)
  }

  pub fn is_empty(&self) -> bool {
    self.audio.is_empty() && self.video.is_empty()
  }

  /// Datagrams dropped since the start, for the metrics.
  pub fn dropped_packets(&self) -> u64 {
    self.dropped_packets
  }

  pub fn queued_bytes(&self) -> usize {
    self.queued_bytes
  }

  fn drop_stale(&mut self, now: Instant) {
    let max_age = self.config.max_age;
    let is_stale = |queued: &Queued| now.saturating_duration_since(queued.queued_at) >= max_age;
    for queue in [&mut self.audio, &mut self.video] {
      while queue.front().is_some_and(is_stale) {
        if let Some(dropped) = queue.pop_front() {
          self.queued_bytes -= dropped.bytes.len();
          self.dropped_packets += 1;
        }
      }
    }
  }

  fn forget(&mut self, dropped: &Queued) {
    self.queued_bytes -= dropped.bytes.len();
    self.dropped_packets += 1;
  }
}

/// Whether a browser's WebSocket with this much buffered should take this datagram.
pub fn may_buffer(buffered_bytes: u32, datagram: &[u8]) -> bool {
  let limit = if is_audio(datagram) {
    BROWSER_AUDIO_BUFFER_BYTES
  } else {
    BROWSER_VIDEO_BUFFER_BYTES
  };
  buffered_bytes < limit
}

/// Whether a datagram carries audio. A datagram this can't read goes with the video, dropped first.
fn is_audio(datagram: &[u8]) -> bool {
  let Ok(header) = DatagramHeader::parse(datagram) else {
    return false;
  };
  datagram
    .get(header.encoded_len() + RTP_PAYLOAD_TYPE_OFFSET)
    .is_some_and(|byte| byte & RTP_PAYLOAD_TYPE_MASK == PayloadType::OPUS.value())
}

#[cfg(test)]
mod tests {
  use super::*;

  const AUDIO_BYTES: usize = 100;
  const VIDEO_BYTES: usize = 1000;

  mod bounds {
    use super::*;

    #[test]
    fn the_bytes_waiting_stay_within_the_bound() {
      let start = Instant::now();
      let mut queue = queue(10 * VIDEO_BYTES, Duration::from_secs(60));
      for _ in 0..1000 {
        queue.push(video(), start);
        assert!(queue.queued_bytes() <= 10 * VIDEO_BYTES);
      }
      assert_eq!(queue.dropped_packets(), 990);
    }

    #[test]
    fn video_goes_before_audio_when_the_queue_is_full() {
      let start = Instant::now();
      let mut queue = queue(VIDEO_BYTES + AUDIO_BYTES, Duration::from_secs(60));
      queue.push(audio(), start);
      queue.push(video(), start);
      queue.push(audio(), start);
      assert_eq!(queue.pop(start).map(|bytes| bytes.len()), Some(AUDIO_BYTES));
      assert_eq!(queue.pop(start).map(|bytes| bytes.len()), Some(AUDIO_BYTES));
      assert_eq!(queue.pop(start), None);
    }

    #[test]
    fn audio_alone_stays_bounded_too() {
      let start = Instant::now();
      let mut queue = queue(3 * AUDIO_BYTES, Duration::from_secs(60));
      for _ in 0..10 {
        queue.push(audio(), start);
      }
      assert_eq!(queue.queued_bytes(), 3 * AUDIO_BYTES);
    }

    #[test]
    fn a_datagram_bigger_than_the_whole_queue_is_dropped_alone() {
      let start = Instant::now();
      let mut queue = queue(AUDIO_BYTES, Duration::from_secs(60));
      queue.push(audio(), start);
      queue.push(video(), start);
      assert_eq!(queue.dropped_packets(), 1);
      assert_eq!(queue.queued_bytes(), AUDIO_BYTES);
    }
  }

  mod staleness {
    use super::*;

    #[test]
    fn what_waited_too_long_is_not_sent() {
      let start = Instant::now();
      let mut queue = queue(1024 * 1024, Duration::from_millis(300));
      queue.push(video(), start);
      queue.push(audio(), start);
      let later = start + Duration::from_millis(300);
      assert_eq!(queue.pop(later), None);
      assert_eq!(queue.dropped_packets(), 2);
      assert_eq!(queue.queued_bytes(), 0);
      assert!(queue.is_empty());
    }

    #[test]
    fn what_is_recent_is_kept_behind_what_went_stale() {
      let start = Instant::now();
      let mut queue = queue(1024 * 1024, Duration::from_millis(300));
      queue.push(video(), start);
      queue.push(video(), start + Duration::from_millis(200));
      assert!(queue.pop(start + Duration::from_millis(400)).is_some());
      assert_eq!(queue.dropped_packets(), 1);
    }

    #[test]
    fn a_stalled_link_never_builds_up_more_than_the_age_allows() {
      let start = Instant::now();
      let mut queue = queue(100 * 1024 * 1024, Duration::from_millis(300));
      // 20 ms apart: 15 fit into the age bound however long the link stays stalled.
      for tick in 0..1000u32 {
        queue.push(audio(), start + Duration::from_millis(20) * tick);
      }
      assert!(
        queue.queued_bytes() <= 15 * AUDIO_BYTES,
        "{}",
        queue.queued_bytes()
      );
    }
  }

  mod order {
    use super::*;

    #[test]
    fn audio_overtakes_the_video_ahead_of_it() {
      let start = Instant::now();
      let mut queue = queue(1024 * 1024, Duration::from_secs(60));
      queue.push(video(), start);
      queue.push(audio(), start);
      assert_eq!(queue.pop(start).map(|bytes| bytes.len()), Some(AUDIO_BYTES));
      assert_eq!(queue.pop(start).map(|bytes| bytes.len()), Some(VIDEO_BYTES));
    }

    #[test]
    fn each_kind_keeps_its_order() {
      let start = Instant::now();
      let mut queue = queue(1024 * 1024, Duration::from_secs(60));
      for marker in 0..3u8 {
        let mut bytes = video();
        *bytes.last_mut().unwrap() = marker;
        queue.push(bytes, start);
      }
      for marker in 0..3u8 {
        assert_eq!(queue.pop(start).unwrap().last(), Some(&marker));
      }
    }
  }

  mod browser_buffer {
    use super::*;

    #[test]
    fn video_stops_first_and_audio_later() {
      assert!(may_buffer(0, &video()));
      assert!(!may_buffer(BROWSER_VIDEO_BUFFER_BYTES, &video()));
      assert!(may_buffer(BROWSER_VIDEO_BUFFER_BYTES, &audio()));
      assert!(!may_buffer(BROWSER_AUDIO_BUFFER_BYTES, &audio()));
    }
  }

  mod hostile_input {
    use super::*;

    #[test]
    fn what_is_no_datagram_counts_as_video() {
      for garbage in [&b""[..], b"\xFF", &[0u8; 6], &[0xC0; 100]] {
        assert!(!is_audio(garbage));
      }
    }

    #[test]
    fn a_header_alone_is_no_audio() {
      assert!(!is_audio(&[0, 0, 0, 0, 1]));
      assert!(!is_audio(&[0, 0, 0, 0, 1, 0x80]));
    }
  }

  fn queue(max_bytes: usize, max_age: Duration) -> MediaQueue {
    MediaQueue::new(MediaQueueConfig { max_bytes, max_age })
  }

  fn audio() -> Vec<u8> {
    datagram(PayloadType::OPUS.value(), AUDIO_BYTES)
  }

  fn video() -> Vec<u8> {
    datagram(100, VIDEO_BYTES)
  }

  /// A 5-byte datagram header without a transport sequence number, then an RTP header's start.
  fn datagram(payload_type: u8, length: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; length];
    bytes[5] = 0x80;
    bytes[6] = payload_type;
    bytes
  }
}
