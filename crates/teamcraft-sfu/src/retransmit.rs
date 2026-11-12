//! Resending video packets a receiver missed (#1762).
//!
//! On a lossy downlink a keyframe of 25 datagrams arrived whole only about one time in seven at
//! 7.5–17 % loss, and its replacement was lost the same way: the picture froze. The SFU keeps the
//! video datagrams it forwarded for a short while ([`PacketHistory`], per track and layer) and
//! sends those a receiver names in a [`teamcraft_proto::CallClientMessage::Nack`] once more, to
//! that receiver only.
//!
//! - **Nothing new to the SFU:** it resends the datagrams exactly as it forwarded them, still DAVE
//!   encrypted, restamped for the receiver's bandwidth estimation like every other. It reads no
//!   more than the RTP header it already routes by.
//! - **Bounded:** the history holds [`RetransmitConfig::window`] of packets, at most
//!   [`RetransmitConfig::max_packets_per_layer`] per layer. What a receiver gets resent comes out
//!   of a budget ([`RetransmitBudget`]) that refills at a share of its downlink estimate, so a
//!   lossy receiver can't make the SFU flood its own link; resent datagrams count in the estimate
//!   like any others.
//! - **Video only:** audio has Opus' own concealment and in-band FEC, and a resent 20 ms frame
//!   would come too late to be played.
//! - **The SFU alone:** losses on a publisher's uplink aren't repaired here; the publisher's
//!   uplink estimate drops its layers instead, and a receiver asks for a keyframe once it gives up
//!   waiting. Most viewers sit behind worse links than the presenter, and only the SFU can repair
//!   one viewer's loss without sending it to everybody.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// What the SFU keeps and resends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetransmitConfig {
  /// How long a forwarded video packet stays available; announced to clients in `joined`, so they
  /// don't ask for older ones. Zero turns retransmission off (and announces nothing).
  pub window: Duration,
  /// Packets kept per track and layer at most: a 1440p screen's keyframe is about 400, a second of
  /// its top layer about as many.
  pub max_packets_per_layer: usize,
  /// The share of a receiver's downlink estimate, in percent, that resent packets may take beyond
  /// [`Self::burst_bytes`].
  pub max_share_percent: u8,
  /// What a receiver may get resent at once after a quiet while: about the lost part of a large
  /// keyframe.
  pub burst_bytes: u32,
}

/// The video datagrams of one layer forwarded within the window, oldest first.
#[derive(Debug, Default)]
pub struct PacketHistory {
  packets: VecDeque<StoredPacket>,
}

#[derive(Debug)]
struct StoredPacket {
  sequence_number: u16,
  arrived: Instant,
  datagram: Vec<u8>,
}

/// The bytes a receiver may get resent: a token bucket that refills at a share of its downlink
/// estimate.
#[derive(Debug, Default)]
pub struct RetransmitBudget {
  bytes: f64,
  maybe_last_refill: Option<Instant>,
}

impl Default for RetransmitConfig {
  fn default() -> Self {
    Self {
      window: Duration::from_secs(1),
      max_packets_per_layer: 1024,
      max_share_percent: 30,
      burst_bytes: 64 * 1024,
    }
  }
}

impl RetransmitConfig {
  /// Whether packets are kept and resent at all.
  pub fn is_enabled(&self) -> bool {
    !self.window.is_zero() && self.max_packets_per_layer > 0
  }

  /// The window as `joined` announces it; `None` when retransmission is off.
  pub fn announced_window_ms(&self) -> Option<u32> {
    self
      .is_enabled()
      .then(|| u32::try_from(self.window.as_millis()).unwrap_or(u32::MAX))
  }
}

impl PacketHistory {
  /// Keeps a forwarded datagram (as it arrived from the publisher) under its RTP sequence number.
  pub fn push(&mut self, sequence_number: u16, datagram: &[u8], now: Instant, config: &RetransmitConfig) {
    if !config.is_enabled() {
      return;
    }
    self.packets.push_back(StoredPacket {
      sequence_number,
      arrived: now,
      datagram: datagram.to_vec(),
    });
    while self.packets.len() > config.max_packets_per_layer
      || self
        .packets
        .front()
        .is_some_and(|oldest| now.saturating_duration_since(oldest.arrived) > config.window)
    {
      self.packets.pop_front();
    }
  }

  /// The datagram with `sequence_number`, if it arrived within the window.
  pub fn get(&self, sequence_number: u16, now: Instant, window: Duration) -> Option<&[u8]> {
    let oldest = self.packets.front()?;
    // Packets are kept in arrival order, which is sequence order but for reordering on the
    // publisher's uplink: look where the number says first, then everywhere.
    let offset = usize::from(sequence_number.wrapping_sub(oldest.sequence_number));
    let packet = self
      .packets
      .get(offset)
      .filter(|packet| packet.sequence_number == sequence_number)
      .or_else(|| {
        self
          .packets
          .iter()
          .find(|packet| packet.sequence_number == sequence_number)
      })?;
    (now.saturating_duration_since(packet.arrived) <= window).then_some(packet.datagram.as_slice())
  }

  pub fn len(&self) -> usize {
    self.packets.len()
  }

  pub fn is_empty(&self) -> bool {
    self.packets.is_empty()
  }
}

impl RetransmitBudget {
  /// Takes `bytes` from the budget if they are there; the budget refills at
  /// [`RetransmitConfig::max_share_percent`] of `estimate_bps` up to the burst.
  pub fn try_spend(
    &mut self,
    bytes: usize,
    estimate_bps: u32,
    now: Instant,
    config: &RetransmitConfig,
  ) -> bool {
    let burst = f64::from(config.burst_bytes);
    let refilled = match self.maybe_last_refill {
      None => burst,
      Some(last) => {
        let bytes_per_sec = f64::from(estimate_bps) / 8.0 * f64::from(config.max_share_percent) / 100.0;
        self.bytes + now.saturating_duration_since(last).as_secs_f64() * bytes_per_sec
      },
    };
    self.bytes = refilled.min(burst);
    self.maybe_last_refill = Some(now);
    let cost = bytes as f64;
    if self.bytes < cost {
      return false;
    }
    self.bytes -= cost;
    true
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const DATAGRAM_BYTES: usize = 1200;

  mod history {
    use super::*;

    #[test]
    fn keeps_packets_by_sequence_number_across_the_wrap() {
      let (mut history, now) = (PacketHistory::default(), Instant::now());
      for sequence_number in [65_534u16, 65_535, 0, 1] {
        history.push(sequence_number, &[sequence_number as u8], now, &config());
      }
      assert_eq!(history.get(65_535, now, config().window), Some(&[255u8][..]));
      assert_eq!(history.get(1, now, config().window), Some(&[1u8][..]));
      assert_eq!(history.get(2, now, config().window), None);
    }

    #[test]
    fn finds_packets_that_arrived_out_of_order() {
      let (mut history, now) = (PacketHistory::default(), Instant::now());
      for sequence_number in [10u16, 12, 11, 13] {
        history.push(sequence_number, &[sequence_number as u8], now, &config());
      }
      for sequence_number in 10u16..=13 {
        assert_eq!(
          history.get(sequence_number, now, config().window),
          Some(&[sequence_number as u8][..])
        );
      }
    }

    #[test]
    fn holds_at_most_the_configured_number_of_packets() {
      let (mut history, now) = (PacketHistory::default(), Instant::now());
      let config = RetransmitConfig {
        max_packets_per_layer: 4,
        ..config()
      };
      for sequence_number in 0u16..10 {
        history.push(sequence_number, &[0; DATAGRAM_BYTES], now, &config);
      }
      assert_eq!(history.len(), 4);
      assert!(history.get(5, now, config.window).is_none());
      assert!(history.get(6, now, config.window).is_some());
    }

    #[test]
    fn forgets_packets_older_than_the_window() {
      let (mut history, start) = (PacketHistory::default(), Instant::now());
      history.push(1, &[1], start, &config());
      let later = start + config().window + Duration::from_millis(1);
      assert!(
        history.get(1, later, config().window).is_none(),
        "too old to resend"
      );
      history.push(2, &[2], later, &config());
      assert_eq!(history.len(), 1, "the old packet is gone");
    }

    #[test]
    fn keeps_nothing_when_retransmission_is_off() {
      let mut history = PacketHistory::default();
      let off = RetransmitConfig {
        window: Duration::ZERO,
        ..config()
      };
      history.push(1, &[1], Instant::now(), &off);
      assert!(history.is_empty());
      assert_eq!(off.announced_window_ms(), None);
      assert_eq!(config().announced_window_ms(), Some(1000));
    }
  }

  mod budget {
    use super::*;

    #[test]
    fn starts_with_the_burst_and_refills_at_a_share_of_the_estimate() {
      let (mut budget, start) = (RetransmitBudget::default(), Instant::now());
      let config = RetransmitConfig {
        burst_bytes: 10 * DATAGRAM_BYTES as u32,
        max_share_percent: 10,
        ..config()
      };
      let resent = (0..100)
        .take_while(|_| budget.try_spend(DATAGRAM_BYTES, 1_000_000, start, &config))
        .count();
      assert_eq!(resent, 10, "the burst");
      // 10 % of 1 Mbit/s is 12.5 kB/s: ten datagrams in about a second.
      let second_later = start + Duration::from_secs(1);
      let resent = (0..100)
        .take_while(|_| budget.try_spend(DATAGRAM_BYTES, 1_000_000, second_later, &config))
        .count();
      assert_eq!(resent, 10);
    }

    #[test]
    fn a_long_quiet_while_refills_no_more_than_the_burst() {
      let (mut budget, start) = (RetransmitBudget::default(), Instant::now());
      let config = RetransmitConfig {
        burst_bytes: 2 * DATAGRAM_BYTES as u32,
        ..config()
      };
      assert!(budget.try_spend(DATAGRAM_BYTES, 1_000_000, start, &config));
      let much_later = start + Duration::from_secs(3600);
      let resent = (0..100)
        .take_while(|_| budget.try_spend(DATAGRAM_BYTES, 1_000_000, much_later, &config))
        .count();
      assert_eq!(resent, 2);
    }
  }

  fn config() -> RetransmitConfig {
    RetransmitConfig::default()
  }
}
