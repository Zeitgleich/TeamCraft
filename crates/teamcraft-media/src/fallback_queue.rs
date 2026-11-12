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
//! Video is dropped by frames, not by packets (#1845): a frame missing a fragment can't be decoded,
//! and every delta frame after it refers to it. So when a video datagram goes, the rest of its
//! layer goes with it up to the next keyframe, also what is queued later, and the layer's stream
//! is noted for a keyframe request ([`MediaQueue::take_keyframe_needs`]): sending frames the
//! receiver can't show only takes the room the keyframe needs.
//!
//! Pure state: time is passed in, so all of it is tested without a network.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::time::Duration;

// `std::time` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::datagram::{DatagramHeader, SimulcastLayer, StreamId};
use crate::fragment::FrameFragment;
use crate::rtp::{PayloadType, RtpHeader};

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
  /// Video layers that lost a frame: what follows is dropped up to their next keyframe.
  broken_layers: HashSet<(StreamId, SimulcastLayer)>,
  /// Streams that lost a frame since [`MediaQueue::take_keyframe_needs`] was last asked.
  keyframe_needs: BTreeSet<StreamId>,
}

#[derive(Debug)]
struct Queued {
  queued_at: Instant,
  bytes: Vec<u8>,
  /// The video frame it carries a part of; `None` for audio and what can't be read.
  maybe_frame: Option<FramePart>,
}

/// Which frame of which layer a video datagram carries a fragment of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FramePart {
  stream_id: StreamId,
  layer: SimulcastLayer,
  /// It is a keyframe's first fragment: the layer can be decoded again from here.
  starts_keyframe: bool,
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
      broken_layers: HashSet::new(),
      keyframe_needs: BTreeSet::new(),
    }
  }

  /// Queues one datagram, dropping what doesn't fit: what is stale first, then the oldest video,
  /// then the oldest audio. Video of a layer that lost a frame is dropped up to its next keyframe.
  pub fn push(&mut self, bytes: Vec<u8>, now: Instant) {
    let is_audio = is_audio(&bytes);
    let maybe_frame = if is_audio { None } else { frame_part(&bytes) };
    if bytes.len() > self.config.max_bytes {
      self.dropped_packets += 1;
      if let Some(frame) = maybe_frame {
        self.break_layer(frame);
      }
      return;
    }
    self.drop_stale(now);
    if let Some(frame) = maybe_frame
      && self.broken_layers.contains(&(frame.stream_id, frame.layer))
    {
      if !frame.starts_keyframe {
        self.dropped_packets += 1;
        return;
      }
      self.broken_layers.remove(&(frame.stream_id, frame.layer));
    }
    self.queued_bytes += bytes.len();
    let queue = if is_audio {
      &mut self.audio
    } else {
      &mut self.video
    };
    queue.push_back(Queued {
      queued_at: now,
      bytes,
      maybe_frame,
    });
    while self.queued_bytes > self.config.max_bytes {
      if let Some(dropped) = self.video.pop_front() {
        self.drop_video(&dropped);
      } else if let Some(dropped) = self.audio.pop_front() {
        self.forget(&dropped);
      } else {
        return;
      }
    }
  }

  /// The streams that lost a frame since the last call: each needs a keyframe to be shown again.
  pub fn take_keyframe_needs(&mut self) -> Vec<StreamId> {
    std::mem::take(&mut self.keyframe_needs).into_iter().collect()
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

  /// Like [`Self::pop`], but video only if `may_send_video`: audio isn't paced (#1810).
  pub fn pop_paced(&mut self, may_send_video: bool, now: Instant) -> Option<Vec<u8>> {
    self.drop_stale(now);
    if self.audio.is_empty() && !may_send_video {
      return None;
    }
    self.pop(now)
  }

  /// Whether audio is waiting (it may go at once, paced or not).
  pub fn has_audio(&self) -> bool {
    !self.audio.is_empty()
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
    while self.audio.front().is_some_and(is_stale) {
      if let Some(dropped) = self.audio.pop_front() {
        self.forget(&dropped);
      }
    }
    while self.video.front().is_some_and(is_stale) {
      if let Some(dropped) = self.video.pop_front() {
        self.drop_video(&dropped);
      }
    }
  }

  /// A video datagram was dropped: so is the rest of its layer up to its next keyframe.
  fn drop_video(&mut self, dropped: &Queued) {
    self.forget(dropped);
    if let Some(frame) = dropped.maybe_frame {
      self.break_layer(frame);
    }
  }

  /// `frame`'s layer lost a part: what is queued of it goes up to a keyframe that starts there, and
  /// what comes later until one does; its stream needs a keyframe.
  fn break_layer(&mut self, frame: FramePart) {
    let key = (frame.stream_id, frame.layer);
    let mut is_broken = true;
    let mut kept = VecDeque::with_capacity(self.video.len());
    for queued in std::mem::take(&mut self.video) {
      let is_of_layer = queued
        .maybe_frame
        .is_some_and(|part| (part.stream_id, part.layer) == key);
      if is_of_layer && is_broken && queued.maybe_frame.is_some_and(|part| part.starts_keyframe) {
        is_broken = false;
      }
      if is_of_layer && is_broken {
        self.forget(&queued);
      } else {
        kept.push_back(queued);
      }
    }
    self.video = kept;
    // A keyframe queued behind the loss already repairs the layer.
    if is_broken {
      self.broken_layers.insert(key);
      self.keyframe_needs.insert(frame.stream_id);
    }
  }

  fn forget(&mut self, dropped: &Queued) {
    self.queued_bytes -= dropped.bytes.len();
    self.dropped_packets += 1;
  }
}

/// The frame a video datagram carries a part of, from its datagram and RTP headers.
fn frame_part(datagram: &[u8]) -> Option<FramePart> {
  let header = DatagramHeader::parse(datagram).ok()?;
  let (rtp, _) = RtpHeader::parse(datagram.get(header.encoded_len()..)?).ok()?;
  let fragment = FrameFragment::from_header(&rtp).ok()?;
  Some(FramePart {
    stream_id: header.stream_id,
    layer: header.layer,
    starts_keyframe: fragment.is_keyframe && fragment.index == 0,
  })
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

  mod pacing {
    use super::*;

    #[test]
    fn audio_goes_while_video_waits_for_the_pacer() {
      let start = Instant::now();
      let mut queue = queue(1024 * 1024, Duration::from_millis(300));
      queue.push(video(), start);
      queue.push(audio(), start);
      assert_eq!(
        queue.pop_paced(false, start).map(|bytes| bytes.len()),
        Some(AUDIO_BYTES)
      );
      assert_eq!(queue.pop_paced(false, start), None);
      assert_eq!(
        queue.pop_paced(true, start).map(|bytes| bytes.len()),
        Some(VIDEO_BYTES)
      );
    }

    #[test]
    fn stale_audio_doesnt_let_video_past_the_pacer() {
      let start = Instant::now();
      let mut queue = queue(1024 * 1024, Duration::from_millis(300));
      queue.push(audio(), start);
      queue.push(video(), start + Duration::from_millis(200));
      assert_eq!(queue.pop_paced(false, start + Duration::from_millis(300)), None);
      assert!(!queue.is_empty());
    }
  }

  /// #1845: video is dropped by frames, and a layer that lost one is dropped up to its next keyframe,
  /// with a keyframe asked for.
  mod frames {
    use super::*;

    #[test]
    fn a_stale_fragment_takes_its_frame_and_the_deltas_after_it_until_a_keyframe() {
      let start = Instant::now();
      let later = start + Duration::from_millis(200);
      let mut queue = queue(1024 * 1024, Duration::from_millis(300));
      queue.push(fragment(7, 0, 1, 0, 2, false), start);
      queue.push(fragment(7, 0, 1, 1, 2, false), later);
      queue.push(fragment(7, 0, 2, 0, 1, false), later);
      // Another layer and another stream are untouched.
      queue.push(fragment(7, 1, 5, 0, 1, false), later);
      queue.push(fragment(8, 0, 9, 0, 1, false), later);
      queue.push(audio(), later);
      // The first fragment went stale: the rest of its frame and the delta after it go too.
      let now = start + Duration::from_millis(300);
      let left: Vec<_> = std::iter::from_fn(|| queue.pop(now)).collect();
      assert_eq!(left.len(), 3, "audio and the other two layers");
      assert_eq!(queue.take_keyframe_needs(), vec![StreamId(7)]);
      assert!(queue.take_keyframe_needs().is_empty(), "asked once");
      // Deltas that come later are dropped as well; the keyframe repairs the layer.
      queue.push(fragment(7, 0, 3, 0, 1, false), now);
      assert!(queue.is_empty());
      queue.push(fragment(7, 0, 4, 0, 2, true), now);
      queue.push(fragment(7, 0, 4, 1, 2, true), now);
      queue.push(fragment(7, 0, 5, 0, 1, false), now);
      assert_eq!(std::iter::from_fn(|| queue.pop(now)).count(), 3);
    }

    #[test]
    fn a_keyframe_queued_behind_the_loss_needs_no_request() {
      let start = Instant::now();
      let mut queue = queue(4 * FRAGMENT_BYTES, Duration::from_secs(60));
      queue.push(fragment(7, 0, 1, 0, 1, false), start);
      queue.push(fragment(7, 0, 2, 0, 2, true), start);
      queue.push(fragment(7, 0, 2, 1, 2, true), start);
      queue.push(fragment(7, 0, 3, 0, 1, false), start);
      // Full: the oldest delta goes, the keyframe behind it stays.
      queue.push(fragment(7, 0, 4, 0, 1, false), start);
      assert_eq!(std::iter::from_fn(|| queue.pop(start)).count(), 4);
      assert!(queue.take_keyframe_needs().is_empty());
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

  const FRAGMENT_BYTES: usize = 600;

  /// A video datagram of `stream`'s `layer` carrying fragment `index` of `count` of frame `frame`.
  fn fragment(stream: u32, layer: u8, frame: u16, index: u16, count: u16, is_keyframe: bool) -> Vec<u8> {
    use crate::rtp::{RtpTimestamp, SequenceNumber, Ssrc};
    let mut bytes = Vec::new();
    DatagramHeader {
      stream_id: StreamId(stream),
      layer: SimulcastLayer::new(layer).unwrap(),
      maybe_transport_seq: None,
    }
    .write_to(&mut bytes);
    let mut rtp = RtpHeader::new(
      PayloadType::new(100).unwrap(),
      SequenceNumber(0),
      RtpTimestamp(0),
      Ssrc(1),
    );
    FrameFragment {
      frame_number: frame,
      index,
      count,
      is_keyframe,
      is_capture_stalled: false,
    }
    .write_to(&mut rtp);
    rtp.write_to(&mut bytes).unwrap();
    bytes.resize(FRAGMENT_BYTES, 0);
    bytes
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
