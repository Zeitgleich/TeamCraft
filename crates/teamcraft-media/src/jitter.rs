//! A packet-count based jitter buffer.
//!
//! Packets are reordered by RTP sequence number. Playout starts once `target_depth_packets` are
//! buffered and then yields one item per call to [`JitterBuffer::pop`], reporting gaps as
//! [`Playout::Missing`] so the decoder can run packet loss concealment or use in-band FEC.
//!
//! Depth is counted in packets instead of wall-clock time: callers pop once per frame interval
//! (20 ms for Opus), which keeps the buffer free of clocks and deterministic to test. Adaptive
//! depth (growing with measured jitter) is a later step.

use std::collections::BTreeMap;

use crate::rtp::SequenceNumber;

/// Sequence numbers more than this far away from the playout position are treated as a stream
/// restart instead of reordering.
const MAX_SEQUENCE_JUMP: u64 = 3000;

/// Reorders packets by sequence number and paces playout.
#[derive(Debug)]
pub struct JitterBuffer<T> {
  config: JitterBufferConfig,
  packets: BTreeMap<u64, T>,
  maybe_highest: Option<u64>,
  /// Extended sequence number of the next slot after the last one played; older packets are late.
  maybe_played_until: Option<u64>,
  state: PlayoutState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JitterBufferConfig {
  /// Packets buffered before playout starts (e.g. 3 × 20 ms = 60 ms).
  pub target_depth_packets: usize,
  /// Upper bound; on overflow the oldest packets are dropped.
  pub max_depth_packets: usize,
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
    }
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
        if self.packets.len() < self.config.target_depth_packets {
          return None;
        }
        *self.packets.keys().next()?
      },
      PlayoutState::Playing { next } => next,
    };
    if self.maybe_highest.is_none_or(|highest| next > highest) {
      // Underrun: re-buffer to rebuild the safety margin.
      self.state = PlayoutState::Buffering;
      return None;
    }
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

  /// Number of packets currently buffered.
  pub fn len(&self) -> usize {
    self.packets.len()
  }

  pub fn is_empty(&self) -> bool {
    self.packets.is_empty()
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
mod tests {
  use super::*;

  const CONFIG: JitterBufferConfig = JitterBufferConfig {
    target_depth_packets: 3,
    max_depth_packets: 8,
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
