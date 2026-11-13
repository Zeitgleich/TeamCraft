//! A packet-count based jitter buffer.
//!
//! Packets are reordered by RTP sequence number. Playout starts once `target_depth_packets` are
//! buffered and then yields one item per call to [`JitterBuffer::pop`], reporting gaps as
//! [`Playout::Missing`] so the decoder can run packet loss concealment or use in-band FEC.
//!
//! Depth is counted in packets instead of wall-clock time: callers pop once per frame interval
//! (20 ms for Opus), which keeps the buffer free of clocks and deterministic to test.
//!
//! The depth adapts per stream (#1894): every underrun deepens it by [`ADAPT_STEP_PACKETS`], up to
//! [`JitterBufferConfig::max_adaptive_depth_packets`], and [`SHRINK_AFTER_POPS`] without one take a
//! packet off again, down to the configured target. A sender on the WebSocket fallback holds its
//! voice back for 100 to 200 ms behind every lost TCP segment and then delivers it in one burst;
//! a receiver on QUIC buffered 60 ms for every stream, and a guest on 0.1 % loss was a gap in
//! almost every second.
//!
//! Network handovers: an outage empties the buffer, which re-buffers to the target depth before
//! playing on. A backlog that arrives in one burst afterwards would add its length as latency for
//! the rest of the call; when the buffer stays well above its target, it skips ahead instead.

use std::collections::BTreeMap;

use crate::rtp::SequenceNumber;

/// Sequence numbers more than this far away from the playout position are treated as a stream
/// restart instead of reordering.
const MAX_SEQUENCE_JUMP: u64 = 3000;
/// Pops in a row (0.5 s of 20 ms frames) the buffer must stay above its excess bound before it
/// skips ahead; a short burst of jitter drains by itself.
const CATCH_UP_AFTER_POPS: u32 = 25;
/// Packets an underrun adds to the depth (40 ms of 20 ms frames).
const ADAPT_STEP_PACKETS: usize = 2;
/// Pops without an underrun (30 s of 20 ms frames) after which the depth gives a packet back.
const SHRINK_AFTER_POPS: u32 = 1500;

/// Reorders packets by sequence number and paces playout.
#[derive(Debug)]
pub struct JitterBuffer<T> {
  config: JitterBufferConfig,
  packets: BTreeMap<u64, T>,
  maybe_highest: Option<u64>,
  /// Extended sequence number of the next slot after the last one played; older packets are late.
  maybe_played_until: Option<u64>,
  state: PlayoutState,
  /// Pops in a row with more than `max_excess_packets` above the target.
  excess_pops: u32,
  /// The depth playout aims for now: the configured target, deepened by underruns.
  target_packets: usize,
  /// Pops since the last underrun or shrink.
  calm_pops: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JitterBufferConfig {
  /// Packets buffered before playout starts (e.g. 3 × 20 ms = 60 ms).
  pub target_depth_packets: usize,
  /// Upper bound; on overflow the oldest packets are dropped.
  pub max_depth_packets: usize,
  /// The most underruns may deepen the target to (#1894): a sender's TCP stall has to fit.
  pub max_adaptive_depth_packets: usize,
  /// Packets above the target that are tolerated for a while; if more stay buffered, playout
  /// skips ahead to the target depth (latency after a handover burst).
  pub max_excess_packets: usize,
}

/// Result of inserting a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
  Inserted,
  Duplicate,
  /// The packet's playout slot has already passed.
  TooLate,
  /// The buffer was full; this many of the oldest packets were dropped.
  Overflowed {
    dropped_packets: usize,
  },
}

/// One playout slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Playout<T> {
  Packet {
    sequence_number: SequenceNumber,
    item: T,
  },
  Missing {
    sequence_number: SequenceNumber,
  },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlayoutState {
  /// Waiting until the target depth is reached.
  Buffering,
  /// `next` is the extended sequence number of the next slot to play.
  Playing { next: u64 },
}

impl Default for JitterBufferConfig {
  fn default() -> Self {
    Self {
      target_depth_packets: 3,
      max_depth_packets: 50,
      max_adaptive_depth_packets: 8,
      max_excess_packets: 10,
    }
  }
}

impl JitterBufferConfig {
  /// For a call on the WebSocket fallback (ARCHITECTURE.md §6.6). TCP loses nothing but delays
  /// everything behind a lost packet, then delivers it in one burst: a stall of a retransmission
  /// (100 to 200 ms on a poor link) has to fit into the buffer, and the burst after it must not count
  /// as a backlog to skip at once. So about three times the depth, at the price of that much
  /// latency, and only on this path.
  pub fn for_fallback() -> Self {
    Self {
      target_depth_packets: 8,
      max_depth_packets: 50,
      max_adaptive_depth_packets: 12,
      max_excess_packets: 17,
    }
  }
}

impl<T> JitterBuffer<T> {
  pub fn new(config: JitterBufferConfig) -> Self {
    Self {
      config,
      packets: BTreeMap::new(),
      maybe_highest: None,
      maybe_played_until: None,
      state: PlayoutState::Buffering,
      excess_pops: 0,
      target_packets: config.target_depth_packets,
      calm_pops: 0,
    }
  }

  /// Another path with other timing: what is buffered stays, and later pops follow the new depths.
  pub fn set_config(&mut self, config: JitterBufferConfig) {
    self.config = config;
    self.excess_pops = 0;
    self.target_packets = self.target_packets.clamp(
      config.target_depth_packets,
      config.max_adaptive_depth_packets.max(config.target_depth_packets),
    );
  }

  /// The depth playout aims for now (packets).
  pub fn target_depth_packets(&self) -> usize {
    self.target_packets
  }

  pub fn push(&mut self, sequence_number: SequenceNumber, item: T) -> PushOutcome {
    let extended = self.extend(sequence_number);
    if self
      .maybe_played_until
      .is_some_and(|played_until| extended < played_until)
    {
      return PushOutcome::TooLate;
    }
    if self.packets.contains_key(&extended) {
      return PushOutcome::Duplicate;
    }
    self.packets.insert(extended, item);
    self.maybe_highest = Some(
      self
        .maybe_highest
        .map_or(extended, |highest| highest.max(extended)),
    );
    self.trim_overflow()
  }

  /// Returns the next playout slot, or `None` while buffering or when the buffer ran dry.
  pub fn pop(&mut self) -> Option<Playout<T>> {
    let next = match self.state {
      PlayoutState::Buffering => {
        if self.packets.len() < self.target_packets {
          return None;
        }
        *self.packets.keys().next()?
      },
      PlayoutState::Playing { next } => self.catch_up(next),
    };
    if self.maybe_highest.is_none_or(|highest| next > highest) {
      // Underrun: re-buffer to rebuild the safety margin, a deeper one from now on.
      self.state = PlayoutState::Buffering;
      self.deepen();
      return None;
    }
    self.note_calm_pop();
    self.state = PlayoutState::Playing { next: next + 1 };
    self.maybe_played_until = Some(next + 1);
    let sequence_number = truncate(next);
    Some(match self.packets.remove(&next) {
      Some(item) => Playout::Packet {
        sequence_number,
        item,
      },
      None => Playout::Missing { sequence_number },
    })
  }

  /// The packet of the slot [`Self::pop`] returns next, if it has arrived: after a
  /// [`Playout::Missing`] it carries the lost packet's in-band FEC data.
  pub fn peek_next(&self) -> Option<&T> {
    match self.state {
      PlayoutState::Playing { next } => self.packets.get(&next),
      PlayoutState::Buffering => None,
    }
  }

  /// Number of packets currently buffered.
  pub fn len(&self) -> usize {
    self.packets.len()
  }

  pub fn is_empty(&self) -> bool {
    self.packets.is_empty()
  }

  /// An underrun: the stream's arrivals vary more than the depth covers.
  fn deepen(&mut self) {
    let ceiling = self
      .config
      .max_adaptive_depth_packets
      .max(self.config.target_depth_packets);
    self.target_packets = (self.target_packets + ADAPT_STEP_PACKETS).min(ceiling);
    self.calm_pops = 0;
  }

  /// A slot played: after long enough without an underrun the depth gives a packet back.
  fn note_calm_pop(&mut self) {
    self.calm_pops += 1;
    if self.calm_pops >= SHRINK_AFTER_POPS {
      self.calm_pops = 0;
      self.target_packets = self
        .target_packets
        .saturating_sub(1)
        .max(self.config.target_depth_packets);
    }
  }

  /// The slot to play: `next`, or further ahead when the buffer has stayed too deep for a while.
  fn catch_up(&mut self, next: u64) -> u64 {
    let Some(highest) = self.maybe_highest else {
      return next;
    };
    let buffered_slots = (highest + 1).saturating_sub(next);
    let excess_bound = (self.target_packets + self.config.max_excess_packets) as u64;
    if buffered_slots <= excess_bound {
      self.excess_pops = 0;
      return next;
    }
    self.excess_pops += 1;
    if self.excess_pops < CATCH_UP_AFTER_POPS {
      return next;
    }
    self.excess_pops = 0;
    let caught_up = (highest + 1).saturating_sub(self.target_packets as u64);
    self.packets.retain(|seq, _| *seq >= caught_up);
    caught_up
  }

  /// Maps a 16-bit sequence number onto a 64-bit counter that doesn't wrap, choosing the value
  /// closest to the highest number seen so far (RFC 3550 appendix A.1).
  fn extend(&mut self, sequence_number: SequenceNumber) -> u64 {
    // Start in the middle of the range so early reordering never underflows.
    const START: u64 = 1 << 32;
    let Some(highest) = self.maybe_highest else {
      return START + u64::from(sequence_number.0);
    };
    let base = highest & !0xFFFF;
    let candidates =
      [base.wrapping_sub(0x1_0000), base, base + 0x1_0000].map(|base| base + u64::from(sequence_number.0));
    let closest = candidates
      .into_iter()
      .min_by_key(|candidate| candidate.abs_diff(highest))
      .unwrap_or(base);
    if closest.abs_diff(highest) > MAX_SEQUENCE_JUMP {
      self.restart();
      return START + u64::from(sequence_number.0);
    }
    closest
  }

  fn restart(&mut self) {
    self.packets.clear();
    self.maybe_highest = None;
    self.maybe_played_until = None;
    self.state = PlayoutState::Buffering;
  }

  fn trim_overflow(&mut self) -> PushOutcome {
    let excess = self.packets.len().saturating_sub(self.config.max_depth_packets);
    if excess == 0 {
      return PushOutcome::Inserted;
    }
    for _ in 0..excess {
      self.packets.pop_first();
    }
    if let (PlayoutState::Playing { next }, Some(&oldest)) = (self.state, self.packets.keys().next()) {
      self.state = PlayoutState::Playing {
        next: next.max(oldest),
      };
    }
    if let Some(&oldest) = self.packets.keys().next() {
      self.maybe_played_until = Some(self.maybe_played_until.map_or(oldest, |until| until.max(oldest)));
    }
    PushOutcome::Overflowed {
      dropped_packets: excess,
    }
  }
}

fn truncate(extended: u64) -> SequenceNumber {
  // Invariant: masking to 16 bits makes the cast lossless.
  SequenceNumber((extended & 0xFFFF) as u16)
}

#[cfg(test)]
mod fallback_timing {
  use super::*;

  /// One retransmission on a TCP path: packets due every 20 ms stop for `stall_pops` slots, then all
  /// arrive at once. How many slots stay empty, and how deep the buffer ends up.
  fn play_through_stall(config: JitterBufferConfig, stall_pops: u16) -> (usize, usize) {
    let mut buffer = JitterBuffer::new(config);
    let mut next = 0u16;
    let mut waiting = 0u16;
    let mut missing = 0;
    for slot in 0u16..300 {
      if (100..100 + stall_pops).contains(&slot) {
        waiting += 1;
      } else {
        for _ in 0..=waiting {
          buffer.push(SequenceNumber(next), next);
          next += 1;
        }
        waiting = 0;
      }
      // From the stall on, a slot without its packet is a gap (buffering again is one too).
      let has_packet = matches!(buffer.pop(), Some(Playout::Packet { .. }));
      if slot >= 100 && !has_packet {
        missing += 1;
      }
    }
    (missing, buffer.packets.len())
  }

  #[test]
  fn a_tcp_stall_that_breaks_the_normal_buffer_plays_through_the_fallbacks() {
    // 140 ms: what a retransmission holds things up on a poor link.
    let (missing_normal, _) = play_through_stall(JitterBufferConfig::default(), 7);
    let (missing_fallback, depth) = play_through_stall(JitterBufferConfig::for_fallback(), 7);
    assert!(missing_normal > 0, "the default depth can't ride this out");
    assert_eq!(missing_fallback, 0);
    assert!(
      depth <= JitterBufferConfig::for_fallback().target_depth_packets,
      "{depth}"
    );
  }

  /// #1894: a receiver on QUIC (the default depth) and a sender on the fallback, whose TCP holds
  /// its voice back for 140 ms every two seconds (a lost segment on a 0.1 % lossy upload). The
  /// first stalls deepen the buffer; after that they play through without a gap.
  #[test]
  fn a_sender_that_stalls_again_and_again_gets_a_deeper_buffer() {
    let mut buffer = JitterBuffer::new(JitterBufferConfig::default());
    let mut next = 0u16;
    let mut waiting = 0u16;
    let mut missing_late = 0;
    for slot in 0u16..1000 {
      if slot % 100 >= 50 && slot % 100 < 57 {
        waiting += 1;
      } else {
        for _ in 0..=waiting {
          buffer.push(SequenceNumber(next), next);
          next += 1;
        }
        waiting = 0;
      }
      let has_packet = matches!(buffer.pop(), Some(Playout::Packet { .. }));
      if slot >= 300 && !has_packet {
        missing_late += 1;
      }
    }
    assert_eq!(missing_late, 0);
    assert!(buffer.target_depth_packets() <= JitterBufferConfig::default().max_adaptive_depth_packets);
  }

  /// A stream that calms down gives the extra depth back, a packet every half minute, down to the
  /// configured target: the latency of one bad moment doesn't stay for the rest of the call.
  #[test]
  fn a_calm_stream_gives_the_depth_back() {
    let mut buffer = JitterBuffer::new(JitterBufferConfig::default());
    let mut next = 0u16;
    // One stall early on.
    for slot in 0u32..120 {
      if !(50..57).contains(&slot) {
        let count = if slot == 57 { 8 } else { 1 };
        for _ in 0..count {
          buffer.push(SequenceNumber(next), next);
          next = next.wrapping_add(1);
        }
      }
      buffer.pop();
    }
    let deepened = buffer.target_depth_packets();
    assert!(
      deepened > JitterBufferConfig::default().target_depth_packets,
      "{deepened}"
    );
    // Ten calm minutes.
    for _ in 0u32..30_000 {
      buffer.push(SequenceNumber(next), next);
      next = next.wrapping_add(1);
      buffer.pop();
    }
    assert_eq!(
      buffer.target_depth_packets(),
      JitterBufferConfig::default().target_depth_packets
    );
  }

  #[test]
  fn a_call_that_moves_to_the_fallback_buffers_more_from_then_on() {
    let mut buffer = JitterBuffer::<u16>::new(JitterBufferConfig::default());
    buffer.set_config(JitterBufferConfig::for_fallback());
    assert_eq!(buffer.config, JitterBufferConfig::for_fallback());
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const CONFIG: JitterBufferConfig = JitterBufferConfig {
    target_depth_packets: 3,
    max_depth_packets: 8,
    max_adaptive_depth_packets: 3,
    max_excess_packets: 10,
  };

  mod playout {
    use super::*;

    #[test]
    fn waits_for_target_depth() {
      let mut buffer = JitterBuffer::new(CONFIG);
      buffer.push(SequenceNumber(10), 10);
      buffer.push(SequenceNumber(11), 11);
      assert_eq!(buffer.pop(), None);
      buffer.push(SequenceNumber(12), 12);
      assert_eq!(drain(&mut buffer), vec![Some(10), Some(11), Some(12)]);
    }

    #[test]
    fn reorders() {
      let mut buffer = JitterBuffer::new(CONFIG);
      for seq in [3, 1, 2, 5, 4] {
        assert_eq!(buffer.push(SequenceNumber(seq), seq), PushOutcome::Inserted);
      }
      assert_eq!(
        drain(&mut buffer),
        vec![Some(1), Some(2), Some(3), Some(4), Some(5)]
      );
    }

    #[test]
    fn reports_gaps() {
      let mut buffer = JitterBuffer::new(CONFIG);
      for seq in [1, 2, 4] {
        buffer.push(SequenceNumber(seq), seq);
      }
      assert_eq!(drain(&mut buffer), vec![Some(1), Some(2), None, Some(4)]);
    }

    #[test]
    fn rebuffers_after_underrun() {
      let mut buffer = JitterBuffer::new(CONFIG);
      for seq in [1, 2, 3] {
        buffer.push(SequenceNumber(seq), seq);
      }
      assert_eq!(drain(&mut buffer).len(), 3);
      buffer.push(SequenceNumber(4), 4);
      assert_eq!(buffer.pop(), None, "one packet is below target depth");
      buffer.push(SequenceNumber(5), 5);
      buffer.push(SequenceNumber(6), 6);
      assert_eq!(drain(&mut buffer), vec![Some(4), Some(5), Some(6)]);
    }

    #[test]
    fn late_packet_after_underrun_is_rejected() {
      let mut buffer = JitterBuffer::new(CONFIG);
      for seq in [1, 3, 4] {
        buffer.push(SequenceNumber(seq), seq);
      }
      assert_eq!(drain(&mut buffer), vec![Some(1), None, Some(3), Some(4)]);
      assert_eq!(buffer.push(SequenceNumber(2), 2), PushOutcome::TooLate);
    }

    #[test]
    fn wraps_around_sequence_space() {
      let mut buffer = JitterBuffer::new(CONFIG);
      for seq in [65534, 0, 65535, 1] {
        buffer.push(SequenceNumber(seq), u32::from(seq));
      }
      let played: Vec<_> = std::iter::from_fn(|| buffer.pop()).collect();
      let sequence_numbers: Vec<_> = played
        .iter()
        .map(|slot| match slot {
          Playout::Packet { sequence_number, .. } | Playout::Missing { sequence_number } => sequence_number.0,
        })
        .collect();
      assert_eq!(sequence_numbers, vec![65534, 65535, 0, 1]);
    }
  }

  mod handover {
    use super::*;

    #[test]
    fn next_packet_is_available_for_fec_after_a_gap() {
      let mut buffer = JitterBuffer::new(CONFIG);
      for seq in [1, 3, 4] {
        buffer.push(SequenceNumber(seq), seq);
      }
      assert_eq!(buffer.peek_next(), None, "nothing before playout starts");
      assert!(matches!(buffer.pop(), Some(Playout::Packet { item: 1, .. })));
      assert!(matches!(buffer.pop(), Some(Playout::Missing { .. })));
      assert_eq!(buffer.peek_next(), Some(&3));
    }

    #[test]
    fn outage_rebuffers_and_a_burst_afterwards_is_skipped_down_to_the_target() {
      let config = JitterBufferConfig {
        target_depth_packets: 3,
        max_depth_packets: 50,
        max_adaptive_depth_packets: 3,
        max_excess_packets: 5,
      };
      let mut buffer = JitterBuffer::new(config);
      let mut seq = 0u16;
      let mut push = |buffer: &mut JitterBuffer<u16>, count: u16| {
        for _ in 0..count {
          buffer.push(SequenceNumber(seq), seq);
          seq += 1;
        }
      };
      push(&mut buffer, 3);
      assert_eq!(drain(&mut buffer).len(), 3);
      // Handover: one second of packets arrives at once.
      push(&mut buffer, 50);
      assert!(buffer.pop().is_some());
      // Playing on in real time (one packet in, one out), the backlog stays at first...
      for _ in 0..CATCH_UP_AFTER_POPS - 1 {
        push(&mut buffer, 1);
        buffer.pop();
      }
      assert!(buffer.len() > 40);
      // ...and is dropped once it persisted: latency is back at the target.
      push(&mut buffer, 1);
      buffer.pop();
      assert_eq!(buffer.len(), 2);
      push(&mut buffer, 1);
      assert!(matches!(buffer.pop(), Some(Playout::Packet { .. })));
    }

    #[test]
    fn short_jitter_bursts_are_kept() {
      let mut buffer = JitterBuffer::new(CONFIG);
      for seq in 0..3 {
        buffer.push(SequenceNumber(seq), seq);
      }
      buffer.pop();
      // Six packets at once, within the excess bound: all of them play.
      for seq in 3..9 {
        buffer.push(SequenceNumber(seq), seq);
      }
      let played: Vec<_> = drain(&mut buffer).into_iter().flatten().collect();
      assert_eq!(played, (1..9).collect::<Vec<_>>());
    }
  }

  mod rejection {
    use super::*;

    #[test]
    fn duplicate() {
      let mut buffer = JitterBuffer::new(CONFIG);
      buffer.push(SequenceNumber(1), 1);
      assert_eq!(buffer.push(SequenceNumber(1), 1), PushOutcome::Duplicate);
    }

    #[test]
    fn too_late() {
      let mut buffer = JitterBuffer::new(CONFIG);
      for seq in [1, 2, 3, 4] {
        buffer.push(SequenceNumber(seq), seq);
      }
      buffer.pop();
      buffer.pop();
      assert_eq!(buffer.push(SequenceNumber(1), 1), PushOutcome::TooLate);
    }

    #[test]
    fn overflow_drops_oldest() {
      let mut buffer = JitterBuffer::new(CONFIG);
      for seq in 0..8 {
        buffer.push(SequenceNumber(seq), seq);
      }
      assert_eq!(
        buffer.push(SequenceNumber(8), 8),
        PushOutcome::Overflowed { dropped_packets: 1 }
      );
      assert_eq!(buffer.len(), 8);
      assert_eq!(drain(&mut buffer).first(), Some(&Some(1)));
    }

    #[test]
    fn large_jump_restarts_stream() {
      let mut buffer = JitterBuffer::new(CONFIG);
      for seq in [1, 2, 3] {
        buffer.push(SequenceNumber(seq), seq);
      }
      buffer.push(SequenceNumber(30_000), 30_000);
      assert_eq!(buffer.len(), 1);
      assert_eq!(buffer.pop(), None);
    }
  }

  /// Pops until the buffer stops yielding; `None` entries are missing slots.
  fn drain<T>(buffer: &mut JitterBuffer<T>) -> Vec<Option<T>> {
    std::iter::from_fn(|| buffer.pop())
      .map(|slot| match slot {
        Playout::Packet { item, .. } => Some(item),
        Playout::Missing { .. } => None,
      })
      .collect()
  }
}
