//! The queue in front of a call's QUIC datagrams (#1218): what waits when the link is too narrow,
//! and what is dropped instead of being sent too late.
//!
//! QUIC sends datagrams as its congestion window allows and keeps the rest in its send buffer,
//! oldest first. When a link's bandwidth drops suddenly, the window is still sized for the old
//! rate, the loss collapses it, and the camera keeps sending at the old rate until the estimators
//! notice. What piled up in QUIC's buffer meanwhile (quinn's holds 1 MiB, seconds at the new rate)
//! went out before anything sent after the drop: the lower layer's keyframe that would bring a
//! limited viewer's picture back waited behind pictures nobody could use any more, or reached a
//! full bottleneck and was lost with them.
//!
//! So QUIC gets more only while it keeps up (`crate::quic_send`: what it holds may take it 50 ms at
//! its current rate, at least [`QUIC_BACKLOG_BYTES`]), and the backlog waits here instead, where it
//! can be judged:
//!
//! - Audio goes first: people follow a call by its sound, and a voice packet is small.
//! - A video frame whose first datagram waited [`SendQueueConfig::max_wait`] is dropped whole: it
//!   would arrive late, and the rest of a frame is no use without its start. A frame whose first
//!   datagram went out is sent to its end: on a narrow link a keyframe takes longer than the wait,
//!   and cut it would only be asked for again.
//! - A keyframe makes what still waits of its picture chain obsolete: the receiver decodes from it
//!   and needs nothing before it. On the uplink the chain is one layer (every layer goes out, each
//!   to other viewers, see [`LayersPerStream`]); on a receiver's downlink it is the whole stream
//!   (the SFU forwards one layer of it at a time, and a switch starts with a keyframe), so the
//!   lower layer's keyframe after a drop goes out at once instead of after the old layer's backlog.
//! - All of it within [`SendQueueConfig::max_bytes`]; when that overflows, the oldest video goes
//!   first.
//!
//! A dropped datagram leaves a gap in the transport sequence numbers, so the bandwidth estimators of
//! `crate::bwe` see it as they see a datagram lost on the way, and one in a layer's RTP sequence
//! numbers, so its receivers ask for a keyframe (unless one follows anyway).
//!
//! Pure state: time is passed in, so all of it is tested without a network, a sudden drop included
//! (see the tests).

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

// `std::time` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::datagram::{DatagramHeader, SimulcastLayer, StreamId};
use crate::fragment::FrameFragment;
use crate::rtp::{PayloadType, RtpHeader};

/// What QUIC may always hold of a call's datagrams while a [`SendQueue`] holds the rest (more while
/// its rate allows, `crate::quic_send`): three packets, about 65 ms at 500 kbit/s. Whatever QUIC
/// holds goes out before anything queued here, also before a keyframe that makes it obsolete.
pub const QUIC_BACKLOG_BYTES: usize = 4 * 1024;

/// How much may wait, and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendQueueConfig {
  /// Bytes waiting, audio and video together: a keyframe of a 1440p screen share and more. A
  /// datagram bigger than this is dropped at once.
  pub max_bytes: usize,
  /// What waited this long before it went out is dropped: audio, a datagram that isn't part of a
  /// frame, and a frame that hasn't started.
  pub max_wait: Duration,
  pub layers_per_stream: LayersPerStream,
}

/// Which layers of a video stream go over the connection, and so what a keyframe makes obsolete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayersPerStream {
  /// All of them (a sender's uplink): each layer is a picture chain of its own.
  All,
  /// One at a time (the SFU to a receiver): a keyframe of any layer starts the stream's chain
  /// anew.
  One,
}

/// Datagrams waiting for QUIC.
#[derive(Debug)]
pub struct SendQueue {
  config: SendQueueConfig,
  audio: VecDeque<Queued>,
  video: VecDeque<Queued>,
  queued_bytes: usize,
  dropped_packets: u64,
  /// Per picture chain (a layer of a stream), the frame being dropped and when: its later
  /// datagrams follow it, also those still to come (the SFU forwards a frame's datagrams as they
  /// arrive).
  dropped_frames: HashMap<Chain, Mark>,
  /// Per picture chain, the last keyframe that made what waited before it obsolete.
  last_keyframes: HashMap<Chain, Mark>,
}

/// A frame number noted at an instant. Frame numbers wrap (after half an hour at 30 fps), so a
/// note is forgotten after [`MARK_LIFETIME`], long after the last datagram of its frame came.
#[derive(Debug, Clone, Copy)]
struct Mark {
  number: u16,
  at: Instant,
}

const MARK_LIFETIME: Duration = Duration::from_secs(10);

/// One layer of one stream.
type Chain = (StreamId, SimulcastLayer);

#[derive(Debug)]
struct Queued {
  queued_at: Instant,
  bytes: Vec<u8>,
  /// The frame it belongs to; `None` for audio and datagrams that aren't part of a frame (the
  /// SFU's padding), which are judged one by one.
  maybe_frame: Option<Frame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Frame {
  chain: Chain,
  number: u16,
  index: u16,
  is_keyframe: bool,
}

/// What a datagram carries, as far as the queue cares.
enum Content {
  Audio,
  Video(Frame),
  /// Not a frame's (the SFU's padding), or nothing the queue can read: judged on its own.
  Other,
}

impl SendQueueConfig {
  /// A sender's uplink: every layer of its tracks.
  pub fn uplink() -> Self {
    Self {
      layers_per_stream: LayersPerStream::All,
      ..Self::downlink()
    }
  }

  /// The SFU's link to one receiver: one layer per stream.
  ///
  /// The wait is about what a home router's queue holds (and the bandwidth tests' bottleneck): with
  /// what QUIC holds and such a queue behind it, nothing goes out that would show a picture more
  /// than about half a second old, also on a 500 kbit/s link.
  pub fn downlink() -> Self {
    Self {
      max_bytes: 1024 * 1024,
      max_wait: Duration::from_millis(250),
      layers_per_stream: LayersPerStream::One,
    }
  }
}

impl SendQueue {
  pub fn new(config: SendQueueConfig) -> Self {
    Self {
      config,
      audio: VecDeque::new(),
      video: VecDeque::new(),
      queued_bytes: 0,
      dropped_packets: 0,
      dropped_frames: HashMap::new(),
      last_keyframes: HashMap::new(),
    }
  }

  /// Queues one datagram, dropping what it makes obsolete and what doesn't fit.
  pub fn push(&mut self, bytes: Vec<u8>, now: Instant) {
    if bytes.len() > self.config.max_bytes {
      self.dropped_packets += 1;
      return;
    }
    let maybe_frame = match content(&bytes) {
      Content::Audio => {
        self.queued_bytes += bytes.len();
        self.audio.push_back(Queued {
          queued_at: now,
          bytes,
          maybe_frame: None,
        });
        self.keep_within_bounds(now);
        return;
      },
      Content::Video(frame) => Some(frame),
      Content::Other => None,
    };
    if let Some(frame) = maybe_frame {
      if is_marked(&self.dropped_frames, &frame, now) {
        // The rest of a frame whose start was dropped.
        self.dropped_packets += 1;
        return;
      }
      if frame.is_keyframe && !is_marked(&self.last_keyframes, &frame, now) {
        self.last_keyframes.insert(
          frame.chain,
          Mark {
            number: frame.number,
            at: now,
          },
        );
        self.drop_before_keyframe(frame, now);
      }
    }
    self.queued_bytes += bytes.len();
    self.video.push_back(Queued {
      queued_at: now,
      bytes,
      maybe_frame,
    });
    self.keep_within_bounds(now);
  }

  /// The next datagram to hand to QUIC: audio first, each kind oldest first, nothing that waited
  /// too long (but the rest of a frame that started).
  pub fn pop(&mut self, now: Instant) -> Option<Vec<u8>> {
    while let Some(queued) = self.audio.pop_front() {
      self.queued_bytes -= queued.bytes.len();
      if self.has_waited_too_long(&queued, now) {
        self.dropped_packets += 1;
        continue;
      }
      return Some(queued.bytes);
    }
    while let Some(queued) = self.video.pop_front() {
      self.queued_bytes -= queued.bytes.len();
      let is_sent = match queued.maybe_frame {
        Some(frame) if is_marked(&self.dropped_frames, &frame, now) => false,
        // The rest of a frame that started (here or before anything had to wait).
        Some(frame) if frame.index > 0 => true,
        Some(frame) if self.has_waited_too_long(&queued, now) => {
          self.dropped_frames.insert(
            frame.chain,
            Mark {
              number: frame.number,
              at: now,
            },
          );
          false
        },
        Some(_) => true,
        None => !self.has_waited_too_long(&queued, now),
      };
      if is_sent {
        return Some(queued.bytes);
      }
      self.dropped_packets += 1;
    }
    let is_current = |mark: &Mark| now.saturating_duration_since(mark.at) < MARK_LIFETIME;
    self.dropped_frames.retain(|_, mark| is_current(mark));
    self.last_keyframes.retain(|_, mark| is_current(mark));
    None
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

  fn has_waited_too_long(&self, queued: &Queued, now: Instant) -> bool {
    now.saturating_duration_since(queued.queued_at) >= self.config.max_wait
  }

  /// Drops what waits of `keyframe`'s chain (on a downlink: of its stream) before it, also the
  /// rest of a frame that started.
  fn drop_before_keyframe(&mut self, keyframe: Frame, now: Instant) {
    let layers_per_stream = self.config.layers_per_stream;
    let is_obsolete = |frame: &Frame| match layers_per_stream {
      LayersPerStream::All => frame.chain == keyframe.chain,
      LayersPerStream::One => frame.chain.0 == keyframe.chain.0,
    } && (frame.chain, frame.number) != (keyframe.chain, keyframe.number);
    let mut dropped_bytes = 0;
    let mut dropped = 0;
    let dropped_frames = &mut self.dropped_frames;
    self.video.retain(|queued| match &queued.maybe_frame {
      Some(frame) if is_obsolete(frame) => {
        dropped_bytes += queued.bytes.len();
        dropped += 1;
        // Datagrams of it that are still to come follow it (frames come in order: the last one
        // noted is the one that may still be coming).
        dropped_frames.insert(
          frame.chain,
          Mark {
            number: frame.number,
            at: now,
          },
        );
        false
      },
      _ => true,
    });
    self.queued_bytes -= dropped_bytes;
    self.dropped_packets += dropped;
  }

  /// Drops the oldest video (a whole frame's worth of it), then the oldest audio, while more than
  /// `max_bytes` wait.
  fn keep_within_bounds(&mut self, now: Instant) {
    while self.queued_bytes > self.config.max_bytes {
      let Some(dropped) = self.video.pop_front().or_else(|| self.audio.pop_front()) else {
        return;
      };
      if let Some(frame) = dropped.maybe_frame {
        self.dropped_frames.insert(
          frame.chain,
          Mark {
            number: frame.number,
            at: now,
          },
        );
      }
      self.queued_bytes -= dropped.bytes.len();
      self.dropped_packets += 1;
    }
  }
}

/// Whether `marks` holds `frame` (of its chain, not yet forgotten).
fn is_marked(marks: &HashMap<Chain, Mark>, frame: &Frame, now: Instant) -> bool {
  marks
    .get(&frame.chain)
    .is_some_and(|mark| mark.number == frame.number && now.saturating_duration_since(mark.at) < MARK_LIFETIME)
}

/// Reads what the queue needs from a datagram's headers (the SFU reads the same ones, never the
/// encrypted payload).
fn content(datagram: &[u8]) -> Content {
  let Ok(header) = DatagramHeader::parse(datagram) else {
    return Content::Other;
  };
  let Some(rtp) = datagram.get(header.encoded_len()..) else {
    return Content::Other;
  };
  let Ok((rtp_header, _)) = RtpHeader::parse(rtp) else {
    return Content::Other;
  };
  if rtp_header.payload_type == PayloadType::OPUS {
    return Content::Audio;
  }
  match FrameFragment::from_header(&rtp_header) {
    Ok(fragment) => Content::Video(Frame {
      chain: (header.stream_id, header.layer),
      number: fragment.frame_number,
      index: fragment.index,
      is_keyframe: fragment.is_keyframe,
    }),
    Err(_) => Content::Other,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::datagram::{MediaDatagram, TransportSeq};
  use crate::rtp::{RtpPacket, RtpTimestamp, SequenceNumber, Ssrc};

  const STREAM: StreamId = StreamId(7);
  const OTHER_STREAM: StreamId = StreamId(8);

  fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
  }

  fn queue(max_bytes: usize, max_wait: Duration, layers_per_stream: LayersPerStream) -> SendQueue {
    SendQueue::new(SendQueueConfig {
      max_bytes,
      max_wait,
      layers_per_stream,
    })
  }

  /// A video datagram: fragment `index` of `count` of frame `number`, `payload_bytes` of payload
  /// whose first byte is `marker`.
  fn video(
    stream: StreamId,
    layer: u8,
    number: u16,
    (index, count): (u16, u16),
    is_keyframe: bool,
    marker: u8,
  ) -> Vec<u8> {
    video_of(
      stream,
      layer,
      number,
      (index, count),
      is_keyframe,
      &[marker; 1000],
    )
  }

  fn video_of(
    stream: StreamId,
    layer: u8,
    number: u16,
    (index, count): (u16, u16),
    is_keyframe: bool,
    payload: &[u8],
  ) -> Vec<u8> {
    let mut header = RtpHeader::new(
      PayloadType::new(96).unwrap(),
      SequenceNumber(number.wrapping_mul(64).wrapping_add(index)),
      RtpTimestamp(u32::from(number) * 3000),
      Ssrc(1),
    );
    FrameFragment {
      frame_number: number,
      index,
      count,
      is_keyframe,
    }
    .write_to(&mut header);
    datagram(stream, layer, &RtpPacket { header, payload })
  }

  fn audio(marker: u8) -> Vec<u8> {
    let header = RtpHeader::new(
      PayloadType::OPUS,
      SequenceNumber(u16::from(marker)),
      RtpTimestamp(0),
      Ssrc(2),
    );
    let payload = [marker; 100];
    datagram(
      StreamId(1),
      0,
      &RtpPacket {
        header,
        payload: &payload,
      },
    )
  }

  fn datagram(stream: StreamId, layer: u8, packet: &RtpPacket<'_>) -> Vec<u8> {
    let rtp = packet.to_bytes().unwrap();
    MediaDatagram {
      header: DatagramHeader {
        stream_id: stream,
        layer: SimulcastLayer::new(layer).unwrap(),
        maybe_transport_seq: Some(TransportSeq(0)),
      },
      rtp: &rtp,
    }
    .to_bytes(usize::MAX)
    .unwrap()
  }

  /// The payload's first byte of what pops, until nothing does.
  fn markers(queue: &mut SendQueue, now: Instant) -> Vec<u8> {
    std::iter::from_fn(|| queue.pop(now))
      .map(|bytes| *bytes.last().unwrap())
      .collect()
  }

  mod order {
    use super::*;

    #[test]
    fn audio_overtakes_the_video_ahead_of_it() {
      let start = Instant::now();
      let mut queue = queue(1 << 20, ms(250), LayersPerStream::All);
      queue.push(video(STREAM, 0, 1, (0, 1), false, 1), start);
      queue.push(audio(2), start);
      assert_eq!(markers(&mut queue, start), [2, 1]);
      assert!(queue.is_empty());
      assert_eq!(queue.queued_bytes(), 0);
    }

    #[test]
    fn video_keeps_its_order() {
      let start = Instant::now();
      let mut queue = queue(1 << 20, ms(250), LayersPerStream::All);
      for (number, marker) in [(1, 1), (2, 2), (3, 3)] {
        queue.push(video(STREAM, 0, number, (0, 1), false, marker), start);
      }
      assert_eq!(markers(&mut queue, start), [1, 2, 3]);
    }
  }

  mod staleness {
    use super::*;

    #[test]
    fn a_frame_that_waited_too_long_is_dropped_whole() {
      let start = Instant::now();
      let mut queue = queue(1 << 20, ms(250), LayersPerStream::All);
      for index in 0..3 {
        queue.push(video(STREAM, 0, 1, (index, 3), false, 1), start);
      }
      queue.push(video(STREAM, 0, 2, (0, 1), false, 2), start + ms(100));
      assert_eq!(markers(&mut queue, start + ms(300)), [2]);
      assert_eq!(queue.dropped_packets(), 3);
      // Its last datagram, still to come (the SFU forwards as they arrive), goes the same way.
      queue.push(video(STREAM, 0, 1, (2, 3), false, 1), start + ms(300));
      assert_eq!(markers(&mut queue, start + ms(300)), Vec::<u8>::new());
    }

    #[test]
    fn a_frame_that_started_is_sent_to_its_end() {
      let start = Instant::now();
      let mut queue = queue(1 << 20, ms(250), LayersPerStream::All);
      for index in 0..3 {
        queue.push(video(STREAM, 0, 1, (index, 3), true, 1), start);
      }
      assert_eq!(
        queue.pop(start + ms(200)).map(|bytes| *bytes.last().unwrap()),
        Some(1)
      );
      // A narrow link takes long for the rest of a keyframe; cut, it would be asked for again.
      assert_eq!(markers(&mut queue, start + ms(900)), [1, 1]);
      assert_eq!(queue.dropped_packets(), 0);
    }

    #[test]
    fn the_rest_of_a_frame_that_started_before_anything_waited_is_sent() {
      let start = Instant::now();
      let mut queue = queue(1 << 20, ms(250), LayersPerStream::All);
      // Its first datagram went to QUIC directly.
      queue.push(video(STREAM, 0, 1, (1, 2), false, 1), start);
      assert_eq!(markers(&mut queue, start + ms(400)), [1]);
    }

    #[test]
    fn a_dropped_frame_is_forgotten_before_its_number_comes_again() {
      let start = Instant::now();
      let mut queue = queue(1 << 20, ms(250), LayersPerStream::All);
      queue.push(video(STREAM, 0, 1, (0, 2), false, 1), start);
      assert_eq!(markers(&mut queue, start + ms(300)), Vec::<u8>::new());
      // Frame numbers wrap after half an hour at 30 fps; by then the frame dropped is long gone.
      let wrapped = start + Duration::from_secs(65_536 / 30);
      queue.push(video(STREAM, 0, 1, (1, 2), false, 2), wrapped);
      assert_eq!(markers(&mut queue, wrapped), [2]);
    }

    #[test]
    fn stale_audio_is_dropped() {
      let start = Instant::now();
      let mut queue = queue(1 << 20, ms(250), LayersPerStream::All);
      queue.push(audio(1), start);
      queue.push(audio(2), start + ms(100));
      assert_eq!(markers(&mut queue, start + ms(300)), [2]);
      assert_eq!(queue.dropped_packets(), 1);
    }
  }

  mod keyframes {
    use super::*;

    #[test]
    fn a_keyframe_drops_what_waits_of_its_layer_before_it_on_the_uplink() {
      let start = Instant::now();
      let mut queue = queue(1 << 20, ms(250), LayersPerStream::All);
      queue.push(video(STREAM, 2, 1, (0, 1), false, 1), start);
      queue.push(video(STREAM, 1, 1, (0, 1), false, 2), start);
      queue.push(video(STREAM, 2, 2, (0, 1), false, 3), start);
      queue.push(video(STREAM, 2, 3, (0, 2), true, 4), start);
      queue.push(video(STREAM, 2, 3, (1, 2), true, 5), start);
      // Other layers go to other viewers: they stay.
      assert_eq!(markers(&mut queue, start), [2, 4, 5]);
      assert_eq!(queue.dropped_packets(), 2);
    }

    #[test]
    fn a_keyframe_of_another_layer_drops_the_stream_s_backlog_on_a_downlink() {
      let start = Instant::now();
      let mut queue = queue(1 << 20, ms(250), LayersPerStream::One);
      queue.push(video(STREAM, 2, 1, (0, 1), false, 1), start);
      queue.push(video(STREAM, 2, 2, (0, 2), false, 1), start);
      queue.push(video(OTHER_STREAM, 2, 1, (0, 1), false, 2), start);
      queue.push(audio(3), start);
      // The receiver was switched to a lower layer: the old one's backlog is of no use any more.
      queue.push(video(STREAM, 0, 9, (0, 2), true, 4), start + ms(10));
      // The rest of the old layer's frame that was cut, forwarded as it arrived, follows it.
      queue.push(video(STREAM, 2, 2, (1, 2), false, 1), start + ms(10));
      queue.push(video(STREAM, 0, 9, (1, 2), true, 5), start + ms(10));
      assert_eq!(markers(&mut queue, start + ms(10)), [3, 2, 4, 5]);
      assert_eq!(queue.dropped_packets(), 3);
    }

    #[test]
    fn the_rest_of_a_keyframe_does_not_drop_its_own_start() {
      let start = Instant::now();
      let mut queue = queue(1 << 20, ms(250), LayersPerStream::One);
      for index in 0..3 {
        queue.push(video(STREAM, 0, 4, (index, 3), true, index as u8), start);
      }
      assert_eq!(markers(&mut queue, start), [0, 1, 2]);
    }
  }

  mod bounds {
    use super::*;

    #[test]
    fn the_bytes_waiting_stay_within_the_bound_oldest_video_first() {
      let start = Instant::now();
      let one = video(STREAM, 0, 1, (0, 1), false, 1).len();
      let mut queue = queue(3 * one, Duration::from_secs(60), LayersPerStream::All);
      queue.push(audio(9), start);
      for number in 1..=4u16 {
        queue.push(video(STREAM, 0, number, (0, 1), false, number as u8), start);
        assert!(queue.queued_bytes() <= 3 * one);
      }
      assert_eq!(markers(&mut queue, start), [9, 3, 4]);
    }

    #[test]
    fn a_datagram_bigger_than_the_whole_queue_is_dropped_alone() {
      let start = Instant::now();
      let mut queue = queue(500, Duration::from_secs(60), LayersPerStream::All);
      queue.push(audio(1), start);
      queue.push(video(STREAM, 0, 1, (0, 1), false, 2), start);
      assert_eq!(queue.dropped_packets(), 1);
      assert_eq!(markers(&mut queue, start), [1]);
    }

    #[test]
    fn what_it_cannot_read_is_judged_on_its_own() {
      let start = Instant::now();
      let mut queue = queue(1 << 20, ms(250), LayersPerStream::One);
      queue.push(vec![0xFF; 10], start);
      queue.push(vec![0xFE; 10], start + ms(200));
      assert_eq!(markers(&mut queue, start + ms(300)), [0xFE]);
    }
  }

  /// The sudden drop of #1218, simulated: a receiver's downlink carries a 720p camera (its top
  /// layer, 1.5 Mbit/s) and drops from 3 Mbit/s to 500 kbit/s at once. The SFU notices after
  /// `REACTION` and switches the receiver to the 180p layer, which starts
  /// with a keyframe.
  ///
  /// QUIC is modelled as a sender at the link's rate (the congestion window at its best) behind
  /// its datagram buffer: quinn's 1 MiB, which drops the oldest when full, before; behind this
  /// queue now, holding [`QUIC_BACKLOG_BYTES`] (it falls behind the link at once). A picture the receiver can decode (a keyframe, or
  /// a delta whose chain is complete) is shown when its last datagram arrives.
  mod sudden_drop {
    use super::*;

    const DROP_AT: Duration = Duration::from_secs(2);
    /// From the drop until the SFU forwards the lower layer: the estimators' cut and the switch
    /// (0.5 to 0.6 s in `bandwidth_limits` on a laptop).
    const REACTION: Duration = Duration::from_millis(600);
    const SWITCH_AT: Duration = Duration::from_millis(2600);
    const END: Duration = Duration::from_secs(6);
    const FRAME_INTERVAL: Duration = Duration::from_micros(33_333);
    const QUINN_DEFAULT_BUFFER: usize = 1024 * 1024;
    const PAYLOAD_BYTES: usize = 1100;

    #[test]
    fn the_lower_layer_shows_at_once_instead_of_after_the_old_one_s_backlog() {
      assert_eq!(SWITCH_AT, DROP_AT + REACTION);
      let after = simulate(true);
      let before = simulate(false);
      let summary = format!("with the queue {after:?}, before {before:?}");
      // Before: the lower layer's keyframe waited behind more than a second of the old layer
      // (1.32 s), and the old layer's pictures came as late (1.35 s).
      assert!(
        before.lower_layer_after_switch > Duration::from_millis(1200),
        "{summary}"
      );
      assert!(before.oldest_picture > Duration::from_millis(1200), "{summary}");
      // Now: it waits behind QUIC's small buffer only (0.12 s), and no picture shown is older
      // than the reaction lets it get (0.59 s).
      assert!(
        after.lower_layer_after_switch < Duration::from_millis(200),
        "{summary}"
      );
      assert!(
        after.oldest_picture < REACTION + Duration::from_millis(100),
        "{summary}"
      );
    }

    #[derive(Debug)]
    struct Outcome {
      /// How old the picture on screen got after the drop at most.
      oldest_picture: Duration,
      /// From the switch until the lower layer's first picture was shown.
      lower_layer_after_switch: Duration,
    }

    /// What the receiver needs to know of a datagram it gets.
    struct Sent {
      captured: Duration,
      is_last: bool,
    }

    /// What the simulated receiver decodes.
    #[derive(Default)]
    struct Receiver {
      /// Per frame (layer, number), its datagrams that arrived.
      arrived: HashMap<(u8, u16), u16>,
      /// The last frame decoded, while its chain is complete.
      maybe_decoded: Option<(u8, u16)>,
      /// When the picture on screen was captured.
      maybe_shown: Option<Duration>,
      oldest_picture: Duration,
      maybe_lower_layer_at: Option<Duration>,
    }

    impl Receiver {
      fn on_datagram(&mut self, frame: &Frame, sent: &Sent, at: Duration) {
        let layer = frame.chain.1.value();
        let count = self.arrived.entry((layer, frame.number)).or_default();
        *count += 1;
        if !sent.is_last || *count != frame.index + 1 {
          return;
        }
        let is_decodable =
          frame.is_keyframe || self.maybe_decoded == Some((layer, frame.number.wrapping_sub(1)));
        if !is_decodable {
          self.maybe_decoded = None;
          return;
        }
        self.maybe_decoded = Some((layer, frame.number));
        self.show(sent.captured, at);
        if layer == 0 && self.maybe_lower_layer_at.is_none() {
          self.maybe_lower_layer_at = Some(at);
        }
      }

      fn show(&mut self, captured: Duration, at: Duration) {
        if let Some(shown) = self.maybe_shown
          && at > DROP_AT
        {
          self.oldest_picture = self.oldest_picture.max(at.saturating_sub(shown));
        }
        self.maybe_shown = Some(captured);
      }
    }

    fn simulate(is_queued: bool) -> Outcome {
      let start = Instant::now();
      let mut queue = SendQueue::new(SendQueueConfig::downlink());
      let quic_buffer_bytes = if is_queued {
        QUIC_BACKLOG_BYTES
      } else {
        QUINN_DEFAULT_BUFFER
      };
      // QUIC's datagram buffer, in order; before, the oldest was dropped when it was full.
      let mut quic: VecDeque<Vec<u8>> = VecDeque::new();
      let mut quic_bytes = 0;
      let mut sent: HashMap<(u8, u16, u16), Sent> = HashMap::new();
      let mut receiver = Receiver::default();
      let mut link_free_at = Duration::ZERO;
      let mut next_frame = Duration::ZERO;
      let mut frame_numbers = [0u16; 3];
      let mut now = Duration::ZERO;
      while now < END {
        if now >= next_frame {
          next_frame += FRAME_INTERVAL;
          let layer = if now < SWITCH_AT { 2 } else { 0 };
          let number = frame_numbers[usize::from(layer)];
          frame_numbers[usize::from(layer)] += 1;
          // Each layer starts with a keyframe.
          let is_keyframe = number == 0;
          let bytes: usize = match (layer, is_keyframe) {
            (2, true) => 30_000,
            (2, false) => 6_250,
            (_, true) => 4_000,
            (_, false) => 390,
          };
          let count = bytes.div_ceil(PAYLOAD_BYTES);
          for index in 0..count as u16 {
            let payload_bytes = if usize::from(index) + 1 == count {
              bytes - PAYLOAD_BYTES * (count - 1)
            } else {
              PAYLOAD_BYTES
            };
            let datagram = video_of(
              STREAM,
              layer,
              number,
              (index, count as u16),
              is_keyframe,
              &vec![0; payload_bytes],
            );
            sent.insert(
              (layer, number, index),
              Sent {
                captured: now,
                is_last: usize::from(index) + 1 == count,
              },
            );
            if !is_queued {
              quic_bytes += datagram.len();
              quic.push_back(datagram);
              while quic_bytes > quic_buffer_bytes {
                quic_bytes -= quic.pop_front().map_or(0, |dropped| dropped.len());
              }
            } else if queue.is_empty() && quic_bytes + datagram.len() <= quic_buffer_bytes {
              quic_bytes += datagram.len();
              quic.push_back(datagram);
            } else {
              queue.push(datagram, start + now);
            }
          }
        }
        // The pump: QUIC takes from the queue as its buffer has room.
        while is_queued && quic_bytes + PAYLOAD_BYTES + 100 <= quic_buffer_bytes {
          let Some(datagram) = queue.pop(start + now) else {
            break;
          };
          quic_bytes += datagram.len();
          quic.push_back(datagram);
        }
        // The link: one datagram after the other at its rate.
        let rate_bps = if now < DROP_AT { 3_000_000.0 } else { 500_000.0 };
        while link_free_at <= now {
          let Some(datagram) = quic.pop_front() else {
            link_free_at = now;
            break;
          };
          quic_bytes -= datagram.len();
          link_free_at += Duration::from_secs_f64(datagram.len() as f64 * 8.0 / rate_bps);
          if let Content::Video(frame) = content(&datagram) {
            let what = &sent[&(frame.chain.1.value(), frame.number, frame.index)];
            receiver.on_datagram(&frame, what, link_free_at);
          }
        }
        now += Duration::from_millis(1);
      }
      if let Some(shown) = receiver.maybe_shown {
        receiver.oldest_picture = receiver.oldest_picture.max(END.saturating_sub(shown));
      }
      Outcome {
        oldest_picture: receiver.oldest_picture,
        lower_layer_after_switch: receiver
          .maybe_lower_layer_at
          .map_or(Duration::MAX, |at| at.saturating_sub(SWITCH_AT)),
      }
    }
  }
}
