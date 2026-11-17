//! A packet-count based jitter buffer.
//!
//! Packets are reordered by RTP sequence number. Playout starts once `target_depth_packets` are
//! buffered and then yields one item per call to [`JitterBuffer::pop`], reporting gaps as
//! [`Playout::Missing`] so the decoder can run packet loss concealment or use in-band FEC.
//!
//! Depth is counted in packets instead of wall-clock time: callers pop once per packet they play
//! (every 20 ms for 20 ms Opus packets), which keeps the buffer free of clocks and deterministic
//! to test. The depths are set in 20 ms frames; a stream of longer packets (a voice on a thin
//! uplink, #1893) says how many frames a packet holds ([`JitterBuffer::set_packet_frames`]), and
//! the depths count the packets that hold as long.
//!
//! The depth adapts per stream (#1894): every underrun deepens it by [`ADAPT_STEP_PACKETS`], up to
//! [`JitterBufferConfig::max_adaptive_depth_packets`], and [`SHRINK_AFTER_POPS`] without one take a
//! packet off again, down to the configured target. A sender on the WebSocket fallback holds its
//! voice back for 100 to 200 ms behind every lost TCP segment and then delivers it in one burst;
//! a receiver on QUIC buffered 60 ms for every stream, and a guest on 0.1 % loss was a gap in
//! almost every second.
//!
//! Underruns come late: the slot is already a gap. A stream whose packets come later and later
//! (voice waiting behind video in a link's queue, #1989) says so before: with arrival times
//! ([`JitterBuffer::push_at`]) the buffer measures how much later than the fastest lately each packet
//! came and deepens to cover it at once. The extra depth is won by playing a concealed frame in place
//! of the next ([`Playout::Stretch`]) while the packets catch up, not by a gap. Video yields to the
//! voice only once even that depth isn't enough (the voice guard's bound).
//!
//! Network handovers: an outage empties the buffer, which re-buffers to the target depth before
//! playing on. A backlog that arrives in one burst afterwards would add its length as latency for
//! the rest of the call; when the buffer stays well above its target, it skips ahead instead.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

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
/// One frame of the depths.
const FRAME: Duration = Duration::from_millis(20);
/// Arrivals within this long count for the lateness the depth covers (#1989).
const LATENESS_WINDOW: Duration = Duration::from_secs(2);
/// At most this many of them are kept: two seconds of 20 ms packets, with room.
const MAX_LATENESS_SAMPLES: usize = 128;
/// No packet for this long (a muted microphone, a pause in a talk): the next starts the lateness
/// afresh. The sequence numbers go on where they stopped, so the pause would read as lateness.
const ARRIVAL_PAUSE: Duration = Duration::from_millis(500);
/// Frames the depth keeps above the lateness measured: lateness that grows (a queue filling up)
/// is measured a packet behind, and the slot it needs is already due.
const LATENESS_HEADROOM_FRAMES: usize = 1;

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
  /// The depth playout aims for now, in 20 ms frames: the configured target, deepened by
  /// underruns.
  target_frames: usize,
  /// Pops since the last underrun or shrink.
  calm_pops: u32,
  /// 20 ms frames per packet of the stream: the depths are divided by it.
  packet_frames: usize,
  /// When the packets arrived, for how late they came (#1989).
  lateness: Lateness,
  /// 20 ms frames the depth grew by lateness that the buffer hasn't won yet: played as
  /// [`Playout::Stretch`] while the buffer is below its target.
  stretch_frames: usize,
}

/// How much later than the fastest lately the packets of a stream arrive: what the depth has to
/// cover so that none comes after its slot.
#[derive(Debug, Default)]
struct Lateness {
  /// The first measured packet: its arrival and extended sequence number, the base of the transit
  /// times.
  maybe_base: Option<(Instant, u64)>,
  /// Recent arrivals: when, and the transit time (arrival minus the packet's place in the stream),
  /// in microseconds; only differences count.
  transits: VecDeque<(Instant, i64)>,
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
  /// No slot is played: a concealed frame as long as a packet goes in between, so the buffer gains
  /// the depth its packets' lateness asks for (#1989). The decoder conceals it like a lost packet.
  Stretch,
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
      target_frames: config.target_depth_packets,
      calm_pops: 0,
      packet_frames: 1,
      lateness: Lateness::default(),
      stretch_frames: 0,
    }
  }

  /// Another path with other timing: what is buffered stays, and later pops follow the new depths.
  pub fn set_config(&mut self, config: JitterBufferConfig) {
    self.config = config;
    self.excess_pops = 0;
    self.target_frames = self.target_frames.clamp(
      config.target_depth_packets,
      config.max_adaptive_depth_packets.max(config.target_depth_packets),
    );
  }

  /// The depth playout aims for now, in 20 ms frames.
  pub fn target_depth_packets(&self) -> usize {
    self.target_frames
  }

  /// The stream's packets hold `frames` 20 ms frames each (1 to 6): the depths, set for 20 ms
  /// packets, count packets of that length from now on, rounded up.
  pub fn set_packet_frames(&mut self, frames: usize) {
    let frames = frames.clamp(1, 6);
    if frames != self.packet_frames {
      // The transit times count the packets' places in the stream by their length.
      self.lateness = Lateness::default();
    }
    self.packet_frames = frames;
  }

  /// [`Self::push`] of a packet that arrived at `arrival`: how late it came deepens the buffer
  /// before its lateness becomes a gap (#1989).
  pub fn push_at(&mut self, sequence_number: SequenceNumber, item: T, arrival: Instant) -> PushOutcome {
    let outcome = self.push(sequence_number, item);
    if outcome == PushOutcome::Duplicate {
      return outcome;
    }
    // Restarted streams are measured afresh, and late packets say the most.
    if let Some(&highest) = self.maybe_highest.as_ref() {
      let extended = self.extend_near(sequence_number, highest);
      self.note_arrival(extended, arrival);
    }
    outcome
  }

  /// Inserts a packet without its arrival time: the depth follows underruns only.
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
        if self.packets.len() < self.target_packets() {
          return None;
        }
        *self.packets.keys().next()?
      },
      PlayoutState::Playing { next } => {
        if self.should_stretch(next) {
          self.stretch_frames = self.stretch_frames.saturating_sub(self.packet_frames);
          return Some(Playout::Stretch);
        }
        self.catch_up(next)
      },
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

  /// A packet of extended number `extended` arrived at `arrival`: its lateness against the fastest
  /// lately, with a packet of its own and [`LATENESS_HEADROOM_FRAMES`], is the depth it needs. More than the target deepens it at
  /// once (to the most underruns may), and the buffer stretches to win the difference.
  fn note_arrival(&mut self, extended: u64, arrival: Instant) {
    let packet = FRAME * u32::try_from(self.packet_frames).unwrap_or(1);
    let Some(lateness) = self.lateness.on_arrival(extended, arrival, packet) else {
      return;
    };
    let ceiling = self
      .config
      .max_adaptive_depth_packets
      .max(self.config.target_depth_packets);
    let late_frames = usize::try_from(lateness.as_micros().div_ceil(FRAME.as_micros())).unwrap_or(usize::MAX);
    if late_frames > ceiling {
      // Later than any depth covers: a stall or a new path, not jitter; measured afresh from here.
      self.lateness.restart_at(extended, arrival);
      return;
    }
    let needed = (late_frames + self.packet_frames + LATENESS_HEADROOM_FRAMES).min(ceiling);
    if needed <= self.target_frames {
      return;
    }
    if self.state != PlayoutState::Buffering {
      self.stretch_frames += needed - self.target_frames;
    }
    self.target_frames = needed;
    self.calm_pops = 0;
  }

  /// Whether to play a stretch in place of slot `next`: the depth grew by lateness, and the
  /// packets buffered from `next` on are fewer than the target. Once they reach it, nothing is
  /// left to win.
  fn should_stretch(&mut self, next: u64) -> bool {
    if self.stretch_frames == 0 {
      return false;
    }
    let buffered = self
      .maybe_highest
      .map_or(0, |highest| (highest + 1).saturating_sub(next));
    if buffered >= self.target_packets() as u64 {
      self.stretch_frames = 0;
      return false;
    }
    true
  }

  /// An underrun: the stream's arrivals vary more than the depth covers.
  fn deepen(&mut self) {
    let ceiling = self
      .config
      .max_adaptive_depth_packets
      .max(self.config.target_depth_packets);
    self.target_frames = (self.target_frames + ADAPT_STEP_PACKETS).min(ceiling);
    self.calm_pops = 0;
    // Re-buffering wins the depth.
    self.stretch_frames = 0;
  }

  /// A slot played: after long enough without an underrun the depth gives a packet back.
  fn note_calm_pop(&mut self) {
    self.calm_pops += 1;
    if self.calm_pops >= SHRINK_AFTER_POPS {
      self.calm_pops = 0;
      self.target_frames = self
        .target_frames
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
    let excess_bound = (self.target_packets() + self.in_packets(self.config.max_excess_packets)) as u64;
    if buffered_slots <= excess_bound {
      self.excess_pops = 0;
      return next;
    }
    self.excess_pops += 1;
    if self.excess_pops < CATCH_UP_AFTER_POPS {
      return next;
    }
    self.excess_pops = 0;
    let caught_up = (highest + 1).saturating_sub(self.target_packets() as u64);
    self.packets.retain(|seq, _| *seq >= caught_up);
    caught_up
  }

  fn target_packets(&self) -> usize {
    self.in_packets(self.target_frames)
  }

  /// `frames` 20 ms frames as packets of the stream, rounded up.
  fn in_packets(&self, frames: usize) -> usize {
    frames.div_ceil(self.packet_frames)
  }

  /// Maps a 16-bit sequence number onto a 64-bit counter that doesn't wrap, choosing the value
  /// closest to the highest number seen so far (RFC 3550 appendix A.1).
  fn extend(&mut self, sequence_number: SequenceNumber) -> u64 {
    // Start in the middle of the range so early reordering never underflows.
    const START: u64 = 1 << 32;
    let Some(highest) = self.maybe_highest else {
      return START + u64::from(sequence_number.0);
    };
    let closest = self.extend_near(sequence_number, highest);
    if closest.abs_diff(highest) > MAX_SEQUENCE_JUMP {
      self.restart();
      return START + u64::from(sequence_number.0);
    }
    closest
  }

  /// `sequence_number` as the extended number closest to `highest`.
  fn extend_near(&self, sequence_number: SequenceNumber, highest: u64) -> u64 {
    let base = highest & !0xFFFF;
    let candidates =
      [base.wrapping_sub(0x1_0000), base, base + 0x1_0000].map(|base| base + u64::from(sequence_number.0));
    candidates
      .into_iter()
      .min_by_key(|candidate| candidate.abs_diff(highest))
      .unwrap_or(base)
  }

  fn restart(&mut self) {
    self.lateness = Lateness::default();
    self.stretch_frames = 0;
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

impl Lateness {
  /// Notes the arrival of packet `extended` at `arrival` (packets `packet` long) and returns how
  /// much later than the fastest within [`LATENESS_WINDOW`] it came; `None` for the first packet
  /// and after a pause in the stream.
  fn on_arrival(&mut self, extended: u64, arrival: Instant, packet: Duration) -> Option<Duration> {
    let is_after_pause = self
      .transits
      .back()
      .is_some_and(|(at, _)| arrival.saturating_duration_since(*at) > ARRIVAL_PAUSE);
    let Some((base_at, base_seq)) = self.maybe_base.filter(|_| !is_after_pause) else {
      self.restart_at(extended, arrival);
      return None;
    };
    let arrived_us =
      i64::try_from(arrival.saturating_duration_since(base_at).as_micros()).unwrap_or(i64::MAX);
    let packet_us = i64::try_from(packet.as_micros()).unwrap_or(i64::MAX);
    // Signed: a packet that arrives late may be numbered before the base.
    let place = i64::try_from(extended).unwrap_or(i64::MAX) - i64::try_from(base_seq).unwrap_or(i64::MAX);
    let transit_us = arrived_us.saturating_sub(place.saturating_mul(packet_us));
    while self
      .transits
      .front()
      .is_some_and(|(at, _)| arrival.saturating_duration_since(*at) > LATENESS_WINDOW)
    {
      self.transits.pop_front();
    }
    if self.transits.len() >= MAX_LATENESS_SAMPLES {
      self.transits.pop_front();
    }
    self.transits.push_back((arrival, transit_us));
    let fastest_us = self
      .transits
      .iter()
      .map(|(_, transit)| *transit)
      .min()
      .unwrap_or(transit_us);
    let late_us = u64::try_from(transit_us.saturating_sub(fastest_us)).unwrap_or(0);
    Some(Duration::from_micros(late_us))
  }

  /// Measures afresh from packet `extended`, arrived at `arrival`.
  fn restart_at(&mut self, extended: u64, arrival: Instant) {
    self.maybe_base = Some((arrival, extended));
    self.transits.clear();
    self.transits.push_back((arrival, 0));
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
mod lateness {
  use super::*;

  /// What a listener hears of a stream played every 20 ms.
  #[derive(Debug, Default)]
  struct Heard {
    /// Slots without sound: an underrun's re-buffering.
    silent: usize,
    /// Slots concealed for a packet that never came in time.
    missing: usize,
    /// Concealed frames the buffer played to win depth.
    stretched: usize,
  }

  /// Packets sent every 20 ms whose delay is `delay_ms(index)`, pushed when they arrive (with
  /// their arrival time when `is_timed`) and played every 20 ms. Counts from the first played slot.
  fn play(is_timed: bool, packets: u16, delay_ms: impl Fn(u16) -> u64) -> (Heard, JitterBuffer<u16>) {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(JitterBufferConfig::default());
    let mut heard = Heard::default();
    let mut next = 0u16;
    let mut has_started = false;
    for tick in 0u64..u64::from(packets) + 50 {
      let now_ms = tick * 20;
      while next < packets && u64::from(next) * 20 + delay_ms(next) <= now_ms {
        let arrival = start + Duration::from_millis(u64::from(next) * 20 + delay_ms(next));
        if is_timed {
          buffer.push_at(SequenceNumber(next), next, arrival);
        } else {
          buffer.push(SequenceNumber(next), next);
        }
        next += 1;
      }
      if next >= packets && buffer.is_empty() {
        break;
      }
      match buffer.pop() {
        Some(Playout::Packet { .. }) => has_started = true,
        Some(Playout::Missing { .. }) => heard.missing += 1,
        Some(Playout::Stretch) => heard.stretched += 1,
        None if has_started && next < packets => heard.silent += 1,
        None => {},
      }
    }
    (heard, buffer)
  }

  /// Voice waits longer and longer behind video in a link's queue (#1989): 100 ms more over a
  /// second. Measured by its lateness, the buffer deepens as the delay grows and wins the depth with
  /// a few concealed frames; counted in underruns only, the listener heard silence.
  #[test]
  fn a_growing_queue_deepens_the_buffer_before_it_runs_dry() {
    let ramp = |index: u16| u64::from(index.saturating_sub(100).min(50)) * 2;
    let (untimed, _) = play(false, 400, ramp);
    let (timed, buffer) = play(true, 400, ramp);
    assert!(untimed.silent > 0, "{untimed:?}");
    assert_eq!(timed.silent, 0, "{timed:?}");
    assert_eq!(timed.missing, 0, "{timed:?}");
    assert!(timed.stretched <= 6, "{timed:?}");
    assert!(
      buffer.target_depth_packets() >= 6,
      "{}",
      buffer.target_depth_packets()
    );
  }

  /// A link whose packets come up to 60 ms apart from their place, at random: the depth covers the
  /// latest, and nothing comes after its slot.
  #[test]
  fn jitter_is_covered_without_gaps() {
    let jitter = |index: u16| u64::from((index.wrapping_mul(37) ^ (index >> 3)) % 4) * 20;
    let (heard, _) = play(true, 500, jitter);
    assert_eq!(heard.missing + heard.silent, 0, "{heard:?}");
  }

  /// A pause in the stream (a muted microphone): the numbers go on where they stopped, which is
  /// no lateness.
  #[test]
  fn a_pause_is_not_lateness() {
    let start = Instant::now();
    let mut buffer = JitterBuffer::new(JitterBufferConfig::default());
    for index in 0u16..50 {
      buffer.push_at(SequenceNumber(index), index, start + FRAME * u32::from(index));
      buffer.pop();
    }
    let later = start + Duration::from_secs(3);
    for index in 50u16..100 {
      buffer.push_at(
        SequenceNumber(index),
        index,
        later + FRAME * u32::from(index - 50),
      );
      buffer.pop();
    }
    assert_eq!(
      buffer.target_depth_packets(),
      JitterBufferConfig::default().target_depth_packets
    );
  }

  /// Lateness beyond any depth (a stall of a second) is no reason to keep the most depth: the
  /// underrun deals with it, and the lateness is measured afresh after it.
  #[test]
  fn a_stall_beyond_every_depth_restarts_the_measure() {
    let stall = |index: u16| {
      if (100..150).contains(&index) {
        1000 - (u64::from(index) - 100) * 20
      } else {
        0
      }
    };
    let (_, buffer) = play(true, 400, stall);
    assert!(
      buffer.target_depth_packets() <= JitterBufferConfig::default().max_adaptive_depth_packets,
      "{}",
      buffer.target_depth_packets()
    );
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

    /// 60 ms packets (#1893): the 60 ms of the target are one packet, not three.
    #[test]
    fn long_packets_wait_for_as_long_not_as_many() {
      let mut buffer = JitterBuffer::new(CONFIG);
      buffer.set_packet_frames(3);
      buffer.push(SequenceNumber(10), 10);
      assert_eq!(drain(&mut buffer), vec![Some(10)]);
      let mut forty = JitterBuffer::new(CONFIG);
      forty.set_packet_frames(2);
      forty.push(SequenceNumber(10), 10);
      assert_eq!(forty.pop(), None, "40 ms is less than the 60 ms of the target");
      forty.push(SequenceNumber(11), 11);
      assert_eq!(drain(&mut forty), vec![Some(10), Some(11)]);
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
          Playout::Stretch => panic!("no stretch without arrival times"),
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
        Playout::Stretch => panic!("no stretch without arrival times"),
      })
      .collect()
  }
}
